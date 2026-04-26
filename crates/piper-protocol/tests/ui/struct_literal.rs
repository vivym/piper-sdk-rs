fn main() {
    let id = piper_protocol::frame::CanId::standard(0x123).unwrap();
    let data = piper_protocol::frame::CanData::new([]).unwrap();
    let _ = piper_protocol::frame::PiperFrame {
        id,
        data,
        timestamp_us: 0,
    };
}
