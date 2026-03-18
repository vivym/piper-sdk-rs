use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_sdk::driver::PiperBuilder as DriverPiperBuilder;

pub fn client_builder(
    interface: Option<&str>,
    serial: Option<&str>,
    daemon_addr: Option<&str>,
) -> ClientPiperBuilder {
    if let Some(addr) = daemon_addr {
        return if addr.starts_with('/') {
            ClientPiperBuilder::new().daemon_uds(addr)
        } else {
            ClientPiperBuilder::new().daemon_udp(addr)
        };
    }

    if let Some(serial) = serial {
        return ClientPiperBuilder::new().gs_usb_serial(serial);
    }

    if let Some(interface) = interface {
        #[cfg(target_os = "linux")]
        {
            return ClientPiperBuilder::new().socketcan(interface);
        }
        #[cfg(not(target_os = "linux"))]
        {
            return ClientPiperBuilder::new().gs_usb_serial(interface);
        }
    }

    #[cfg(target_os = "linux")]
    {
        ClientPiperBuilder::new().socketcan("can0")
    }
    #[cfg(target_os = "macos")]
    {
        ClientPiperBuilder::new().daemon_udp("127.0.0.1:18888")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        ClientPiperBuilder::new()
    }
}

pub fn driver_builder(
    interface: Option<&str>,
    serial: Option<&str>,
    daemon_addr: Option<&str>,
) -> DriverPiperBuilder {
    if let Some(addr) = daemon_addr {
        return if addr.starts_with('/') {
            DriverPiperBuilder::new().daemon_uds(addr)
        } else {
            DriverPiperBuilder::new().daemon_udp(addr)
        };
    }

    if let Some(serial) = serial {
        return DriverPiperBuilder::new().gs_usb_serial(serial);
    }

    if let Some(interface) = interface {
        #[cfg(target_os = "linux")]
        {
            return DriverPiperBuilder::new().socketcan(interface);
        }
        #[cfg(not(target_os = "linux"))]
        {
            return DriverPiperBuilder::new().gs_usb_serial(interface);
        }
    }

    #[cfg(target_os = "linux")]
    {
        DriverPiperBuilder::new().socketcan("can0")
    }
    #[cfg(target_os = "macos")]
    {
        DriverPiperBuilder::new().daemon_udp("127.0.0.1:18888")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        DriverPiperBuilder::new()
    }
}
