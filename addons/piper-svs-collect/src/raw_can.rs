use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Result;

#[cfg(target_os = "linux")]
use {
    anyhow::{Context, anyhow},
    std::ffi::CString,
    std::fs::{self, File, OpenOptions},
    std::io::BufWriter,
    std::mem,
    std::os::fd::RawFd,
    std::path::{Path, PathBuf},
    std::sync::atomic::{AtomicBool, AtomicU64},
    std::thread::{self, JoinHandle},
    std::time::Duration,
};

#[cfg(target_os = "linux")]
use piper_tools::{
    RecordedFrameDirection, RecordingMetadata, TimestampSource, TimestampedFrame,
    recording::v3::StreamingRecordingWriter,
};

#[cfg(all(test, target_os = "linux"))]
use piper_tools::PiperRecording;

#[cfg(target_os = "linux")]
static RAW_CAN_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(not(target_os = "linux"))]
use {anyhow::anyhow, std::path::Path, std::sync::atomic::AtomicBool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RawCanCaptureStatus {
    Disabled = 0,
    Ok = 1,
    Degraded = 2,
    Requested = 3,
}

impl RawCanCaptureStatus {
    pub fn as_step_code(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Ok | Self::Requested => 1,
            Self::Degraded => 2,
        }
    }

    fn storage_code(self) -> u8 {
        self as u8
    }

    pub fn manifest_finalizer_status(self) -> &'static str {
        match self {
            Self::Disabled => "not_enabled",
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Requested => "not_started",
        }
    }
}

impl From<u8> for RawCanCaptureStatus {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Ok,
            2 => Self::Degraded,
            3 => Self::Requested,
            _ => Self::Disabled,
        }
    }
}

pub trait RawCanStatusSource: Send + Sync {
    fn raw_can_status(&self) -> RawCanCaptureStatus;
}

#[derive(Debug)]
pub struct RawCanStatusTracker {
    status: AtomicU8,
}

impl RawCanStatusTracker {
    pub fn disabled() -> Self {
        Self::new(RawCanCaptureStatus::Disabled)
    }

    pub fn ok() -> Self {
        Self::new(RawCanCaptureStatus::Ok)
    }

    pub fn requested() -> Self {
        Self::new(RawCanCaptureStatus::Requested)
    }

    pub fn new(status: RawCanCaptureStatus) -> Self {
        Self {
            status: AtomicU8::new(status.storage_code()),
        }
    }

    pub fn set_status(&self, status: RawCanCaptureStatus) {
        self.status.store(status.storage_code(), Ordering::Release);
    }

    pub fn mark_degraded(&self) {
        self.set_status(RawCanCaptureStatus::Degraded);
    }

    pub fn finalizer_status(&self) -> String {
        self.raw_can_status().manifest_finalizer_status().to_string()
    }
}

impl RawCanStatusSource for RawCanStatusTracker {
    fn raw_can_status(&self) -> RawCanCaptureStatus {
        RawCanCaptureStatus::from(self.status.load(Ordering::Acquire))
    }
}

#[derive(Debug)]
pub struct RawCanRecordingHandle {
    status: Arc<RawCanStatusTracker>,
    #[cfg(target_os = "linux")]
    stop_signal: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    threads: Vec<JoinHandle<()>>,
}

impl RawCanRecordingHandle {
    pub fn start(
        requested: bool,
        episode_dir: &Path,
        master_iface: &str,
        slave_iface: &str,
        external_cancel: Arc<AtomicBool>,
        status: Arc<RawCanStatusTracker>,
    ) -> Result<Option<Self>> {
        if !requested {
            status.set_status(RawCanCaptureStatus::Disabled);
            return Ok(None);
        }

        #[cfg(target_os = "linux")]
        {
            Self::start_linux(
                episode_dir,
                master_iface,
                slave_iface,
                external_cancel,
                status,
            )
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (episode_dir, master_iface, slave_iface, external_cancel);
            status.mark_degraded();
            Err(anyhow!(
                "raw CAN side recording requires Linux SocketCAN packet capture"
            ))
        }
    }

    pub fn stop(self) {
        #[cfg(target_os = "linux")]
        {
            self.stop_signal.store(true, Ordering::Release);
            for thread in self.threads {
                if thread.join().is_err() {
                    self.status.mark_degraded();
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = self.status;
        }
    }

    #[cfg(target_os = "linux")]
    fn start_linux(
        episode_dir: &Path,
        master_iface: &str,
        slave_iface: &str,
        external_cancel: Arc<AtomicBool>,
        status: Arc<RawCanStatusTracker>,
    ) -> Result<Option<Self>> {
        status.set_status(RawCanCaptureStatus::Ok);
        let stop_signal = Arc::new(AtomicBool::new(false));

        let master_capture = RawCanCapture::open(RawCanSide::Master, episode_dir, master_iface)
            .inspect_err(|_| status.mark_degraded())?;
        let slave_capture = RawCanCapture::open(RawCanSide::Slave, episode_dir, slave_iface)
            .inspect_err(|_| status.mark_degraded())?;

        let master_thread = spawn_capture_thread(
            master_capture,
            Arc::clone(&stop_signal),
            Arc::clone(&external_cancel),
            Arc::clone(&status),
        )
        .inspect_err(|_| status.mark_degraded())?;
        let slave_thread = match spawn_capture_thread(
            slave_capture,
            Arc::clone(&stop_signal),
            external_cancel,
            Arc::clone(&status),
        ) {
            Ok(thread) => thread,
            Err(error) => {
                stop_signal.store(true, Ordering::Release);
                let _ = master_thread.join();
                status.mark_degraded();
                return Err(error);
            },
        };

        Ok(Some(Self {
            status,
            stop_signal,
            threads: vec![master_thread, slave_thread],
        }))
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
enum RawCanSide {
    Master,
    Slave,
}

#[cfg(target_os = "linux")]
impl RawCanSide {
    fn label(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Slave => "slave",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Master => "raw_can/master.piperrec",
            Self::Slave => "raw_can/slave.piperrec",
        }
    }
}

#[cfg(target_os = "linux")]
struct RawCanCapture {
    side: RawCanSide,
    socket: PacketCanSocket,
    writer: RawCanRecordingFile,
}

#[cfg(target_os = "linux")]
impl RawCanCapture {
    fn open(side: RawCanSide, episode_dir: &Path, iface: &str) -> Result<Self> {
        std::fs::create_dir_all(episode_dir.join("raw_can")).with_context(|| {
            format!(
                "failed to create raw CAN directory {}",
                episode_dir.join("raw_can").display()
            )
        })?;
        let path = episode_dir.join(side.file_name());
        let socket = PacketCanSocket::open(iface)
            .with_context(|| format!("failed to open raw CAN packet capture on {iface}"))?;
        let writer = RawCanRecordingFile::create(path, iface)?;
        Ok(Self {
            side,
            socket,
            writer,
        })
    }
}

#[cfg(target_os = "linux")]
struct RawCanRecordingFile {
    output_path: PathBuf,
    temp_path: PathBuf,
    writer: Option<StreamingRecordingWriter<BufWriter<File>>>,
}

#[cfg(target_os = "linux")]
impl RawCanRecordingFile {
    fn create(output_path: PathBuf, iface: &str) -> Result<Self> {
        if output_path.try_exists().with_context(|| {
            format!(
                "failed to check raw CAN recording {}",
                output_path.display()
            )
        })? {
            return Err(anyhow!(
                "raw CAN recording already exists: {}",
                output_path.display()
            ));
        }

        let temp_path = raw_can_temp_path(&output_path);
        let file = OpenOptions::new().write(true).create_new(true).open(&temp_path).with_context(
            || {
                format!(
                    "failed to create raw CAN recording temp file {}",
                    temp_path.display()
                )
            },
        )?;
        let metadata = RecordingMetadata::new(iface.to_string(), 0);
        let writer = match StreamingRecordingWriter::new(BufWriter::new(file), &metadata) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            },
        };
        Ok(Self {
            output_path,
            temp_path,
            writer: Some(writer),
        })
    }

    fn push_frame(&mut self, frame: &TimestampedFrame) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow!("raw CAN recording already finalized"))?;
        writer.push_frame(frame)
    }

    fn finish(mut self) -> Result<()> {
        let result = (|| {
            let writer = self
                .writer
                .take()
                .ok_or_else(|| anyhow!("raw CAN recording already finalized"))?;
            let writer = writer.finish()?;
            writer.get_ref().sync_all().with_context(|| {
                format!(
                    "failed to sync raw CAN temp recording {}",
                    self.temp_path.display()
                )
            })?;
            drop(writer);

            persist_temp_no_overwrite(&self.temp_path, &self.output_path).with_context(|| {
                format!(
                    "failed to persist raw CAN recording {}",
                    self.output_path.display()
                )
            })?;
            fsync_parent(&self.output_path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&self.temp_path);
        }
        result
    }
}

#[cfg(target_os = "linux")]
impl Drop for RawCanRecordingFile {
    fn drop(&mut self) {
        if self.writer.is_some() {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

#[cfg(target_os = "linux")]
fn raw_can_temp_path(path: &Path) -> PathBuf {
    let counter = RAW_CAN_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("raw-can");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        counter
    ))
}

#[cfg(target_os = "linux")]
fn persist_temp_no_overwrite(temp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    fs::hard_link(temp_path, final_path)?;
    fs::remove_file(temp_path)
}

#[cfg(target_os = "linux")]
fn fsync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        let dir = fs::File::open(parent)
            .with_context(|| format!("failed to open raw CAN parent dir {}", parent.display()))?;
        dir.sync_all()
            .with_context(|| format!("failed to sync raw CAN parent dir {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn spawn_capture_thread(
    capture: RawCanCapture,
    stop_signal: Arc<AtomicBool>,
    external_cancel: Arc<AtomicBool>,
    status: Arc<RawCanStatusTracker>,
) -> Result<JoinHandle<()>> {
    let name = format!("svs-raw-can-{}", capture.side.label());
    thread::Builder::new()
        .name(name)
        .spawn(move || run_capture_loop(capture, stop_signal, external_cancel, status))
        .context("failed to spawn raw CAN side recording thread")
}

#[cfg(target_os = "linux")]
fn run_capture_loop(
    mut capture: RawCanCapture,
    stop_signal: Arc<AtomicBool>,
    external_cancel: Arc<AtomicBool>,
    status: Arc<RawCanStatusTracker>,
) {
    while !stop_signal.load(Ordering::Acquire) && !external_cancel.load(Ordering::Acquire) {
        match capture.socket.receive() {
            Ok(Some(packet)) => match timestamped_frame_from_packet(packet) {
                Ok(Some(frame)) => {
                    if capture.writer.push_frame(&frame).is_err() {
                        status.mark_degraded();
                        return;
                    }
                },
                Ok(None) => {
                    status.mark_degraded();
                },
                Err(_) => {
                    status.mark_degraded();
                },
            },
            Ok(None) => {},
            Err(_) => {
                status.mark_degraded();
                return;
            },
        }
    }

    if capture.writer.finish().is_err() {
        status.mark_degraded();
    }
}

#[cfg(target_os = "linux")]
fn timestamped_frame_from_packet(packet: RawCanPacket) -> Result<Option<TimestampedFrame>> {
    if packet.error_frame || packet.remote_transmission_request {
        return Ok(None);
    }

    let data = &packet.data[..usize::from(packet.dlc)];
    let frame = if packet.extended {
        piper_sdk::PiperFrame::new_extended(packet.id, data)
    } else {
        piper_sdk::PiperFrame::new_standard(packet.id, data)
    }?
    .with_timestamp_us(piper_driver::heartbeat::monotonic_micros());

    let direction = if packet.packet_type == libc::PACKET_OUTGOING {
        RecordedFrameDirection::Tx
    } else {
        RecordedFrameDirection::Rx
    };
    Ok(Some(TimestampedFrame::new(
        frame,
        direction,
        Some(TimestampSource::Userspace),
    )))
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PacketCanSocket {
    fd: RawFd,
}

#[cfg(target_os = "linux")]
impl PacketCanSocket {
    fn open(iface: &str) -> Result<Self> {
        const ETH_P_CAN: u16 = 0x000C;
        let iface_c = CString::new(iface)
            .with_context(|| format!("SocketCAN interface contains NUL byte: {iface:?}"))?;
        let ifindex = unsafe { libc::if_nametoindex(iface_c.as_ptr()) };
        if ifindex == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to resolve interface index for {iface}"));
        }

        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                i32::from(ETH_P_CAN.to_be()),
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to create packet socket");
        }

        let socket = Self { fd };
        if let Err(error) = socket.set_receive_timeout(Duration::from_millis(10)) {
            return Err(error.context("failed to set packet socket receive timeout"));
        }
        if let Err(error) = socket.bind(ifindex, ETH_P_CAN) {
            return Err(error.context(format!("failed to bind packet socket to {iface}")));
        }
        Ok(socket)
    }

    fn bind(&self, ifindex: u32, eth_protocol: u16) -> Result<()> {
        let mut addr = unsafe { mem::zeroed::<libc::sockaddr_ll>() };
        addr.sll_family = libc::AF_PACKET as u16;
        addr.sll_protocol = eth_protocol.to_be();
        addr.sll_ifindex = i32::try_from(ifindex).context("CAN interface index overflow")?;

        let result = unsafe {
            libc::bind(
                self.fd,
                (&addr as *const libc::sockaddr_ll).cast::<libc::sockaddr>(),
                mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error()).context("packet socket bind failed");
        }
        Ok(())
    }

    fn set_receive_timeout(&self, timeout: Duration) -> Result<()> {
        let timeout = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        let result = unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&timeout as *const libc::timeval).cast::<libc::c_void>(),
                mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error()).context("setsockopt(SO_RCVTIMEO) failed");
        }
        Ok(())
    }

    fn receive(&self) -> Result<Option<RawCanPacket>> {
        let mut buffer = [0_u8; CLASSIC_CAN_FRAME_BYTES];
        let mut addr = unsafe { mem::zeroed::<libc::sockaddr_ll>() };
        let mut addr_len = mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let bytes = unsafe {
            libc::recvfrom(
                self.fd,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
                0,
                (&mut addr as *mut libc::sockaddr_ll).cast::<libc::sockaddr>(),
                &mut addr_len,
            )
        };
        if bytes < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(code)
                    if code == libc::EAGAIN || code == libc::EWOULDBLOCK || code == libc::EINTR
            ) {
                return Ok(None);
            }
            return Err(error).context("packet socket receive failed");
        }
        if bytes as usize != CLASSIC_CAN_FRAME_BYTES {
            return Err(anyhow!(
                "unexpected SocketCAN packet size: expected {CLASSIC_CAN_FRAME_BYTES}, got {bytes}"
            ));
        }

        Ok(Some(RawCanPacket::from_classic_frame_bytes(
            &buffer,
            addr.sll_pkttype,
        )?))
    }
}

#[cfg(target_os = "linux")]
impl Drop for PacketCanSocket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[cfg(target_os = "linux")]
const CLASSIC_CAN_FRAME_BYTES: usize = 16;
#[cfg(target_os = "linux")]
const CAN_EFF_FLAG: u32 = 0x8000_0000;
#[cfg(target_os = "linux")]
const CAN_RTR_FLAG: u32 = 0x4000_0000;
#[cfg(target_os = "linux")]
const CAN_ERR_FLAG: u32 = 0x2000_0000;
#[cfg(target_os = "linux")]
const CAN_SFF_MASK: u32 = 0x0000_07FF;
#[cfg(target_os = "linux")]
const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawCanPacket {
    raw_can_id: u32,
    id: u32,
    extended: bool,
    remote_transmission_request: bool,
    error_frame: bool,
    dlc: u8,
    data: [u8; 8],
    packet_type: u8,
}

#[cfg(target_os = "linux")]
impl RawCanPacket {
    fn from_classic_frame_bytes(
        buffer: &[u8; CLASSIC_CAN_FRAME_BYTES],
        packet_type: u8,
    ) -> Result<Self> {
        let raw_can_id =
            u32::from_ne_bytes(buffer[0..4].try_into().expect("slice length is fixed"));
        let dlc = buffer[4];
        if dlc > 8 {
            return Err(anyhow!("classic CAN DLC is greater than 8: {dlc}"));
        }
        let extended = raw_can_id & CAN_EFF_FLAG != 0;
        let id = if extended {
            raw_can_id & CAN_EFF_MASK
        } else {
            raw_can_id & CAN_SFF_MASK
        };
        let mut data = [0_u8; 8];
        data.copy_from_slice(&buffer[8..16]);
        Ok(Self {
            raw_can_id,
            id,
            extended,
            remote_transmission_request: raw_can_id & CAN_RTR_FLAG != 0,
            error_frame: raw_can_id & CAN_ERR_FLAG != 0,
            dlc,
            data,
            packet_type,
        })
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
fn packet_type_label(packet_type: u8) -> &'static str {
    match packet_type {
        libc::PACKET_HOST => "host",
        libc::PACKET_BROADCAST => "broadcast",
        libc::PACKET_MULTICAST => "multicast",
        libc::PACKET_OTHERHOST => "other_host",
        libc::PACKET_OUTGOING => "outgoing",
        libc::PACKET_LOOPBACK => "loopback",
        _ => "unknown",
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn status_tracker_reports_finalizer_status() {
        let tracker = RawCanStatusTracker::ok();
        assert_eq!(tracker.finalizer_status(), "ok");
        tracker.mark_degraded();
        assert_eq!(tracker.finalizer_status(), "degraded");
    }

    #[test]
    fn requested_status_reports_not_started_without_degradation() {
        let tracker = RawCanStatusTracker::requested();

        assert_eq!(tracker.raw_can_status(), RawCanCaptureStatus::Requested);
        assert_eq!(tracker.raw_can_status().as_step_code(), 1);
        assert_eq!(tracker.finalizer_status(), "not_started");
    }

    #[test]
    fn raw_can_startup_failure_marks_degraded() {
        let episode_dir = tempfile::tempdir().unwrap();
        let status = Arc::new(RawCanStatusTracker::requested());
        let cancel = Arc::new(AtomicBool::new(false));
        let missing_iface = format!("svs-missing-{}", std::process::id());

        let result = RawCanRecordingHandle::start(
            true,
            episode_dir.path(),
            &missing_iface,
            "also-missing",
            cancel,
            Arc::clone(&status),
        );

        assert!(result.is_err());
        assert_eq!(status.raw_can_status(), RawCanCaptureStatus::Degraded);
    }

    #[test]
    fn raw_can_side_paths_match_episode_layout() {
        assert_eq!(RawCanSide::Master.file_name(), "raw_can/master.piperrec");
        assert_eq!(RawCanSide::Slave.file_name(), "raw_can/slave.piperrec");
    }

    #[test]
    fn writes_loadable_piper_recording_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let raw_can_dir = dir.path().join("raw_can");
        fs::create_dir(&raw_can_dir).unwrap();
        let path = raw_can_dir.join("master.piperrec");
        let frame = TimestampedFrame::new(
            piper_sdk::PiperFrame::new_standard(0x123, [1, 2, 3])
                .unwrap()
                .with_timestamp_us(42),
            RecordedFrameDirection::Rx,
            Some(TimestampSource::Userspace),
        );

        let mut writer = RawCanRecordingFile::create(path.clone(), "vcan0").unwrap();
        writer.push_frame(&frame).unwrap();
        writer.finish().unwrap();
        let loaded = PiperRecording::load(&path).unwrap();

        assert_eq!(loaded.frame_count(), 1);
        assert!(RawCanRecordingFile::create(path, "vcan0").is_err());
    }

    #[test]
    fn unfinished_raw_can_recording_removes_temp_file_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let raw_can_dir = dir.path().join("raw_can");
        fs::create_dir(&raw_can_dir).unwrap();
        let path = raw_can_dir.join("master.piperrec");

        let writer = RawCanRecordingFile::create(path.clone(), "vcan0").unwrap();
        let temp_path = writer.temp_path.clone();
        assert!(temp_path.exists());

        drop(writer);

        assert!(!temp_path.exists());
        assert!(!path.exists());
    }

    #[test]
    fn parses_classic_can_frame_bytes() {
        let raw_can_id = CAN_EFF_FLAG | CAN_RTR_FLAG | 0x0012_3456;
        let mut buffer = [0_u8; CLASSIC_CAN_FRAME_BYTES];
        buffer[0..4].copy_from_slice(&raw_can_id.to_ne_bytes());
        buffer[4] = 3;
        buffer[8..16].copy_from_slice(&[1, 2, 3, 0, 0, 0, 0, 0]);

        let packet =
            RawCanPacket::from_classic_frame_bytes(&buffer, libc::PACKET_OUTGOING).unwrap();

        assert_eq!(packet.raw_can_id, raw_can_id);
        assert_eq!(packet.id, 0x0012_3456);
        assert!(packet.extended);
        assert!(packet.remote_transmission_request);
        assert!(!packet.error_frame);
        assert_eq!(packet.dlc, 3);
        assert_eq!(packet.data, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(packet_type_label(packet.packet_type), "outgoing");
    }

    #[test]
    fn rejects_invalid_classic_can_dlc() {
        let mut buffer = [0_u8; CLASSIC_CAN_FRAME_BYTES];
        buffer[4] = 9;

        assert!(RawCanPacket::from_classic_frame_bytes(&buffer, 0).is_err());
    }
}
