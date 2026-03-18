use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_driver::{ConnectionTarget, PiperBuilder as DriverPiperBuilder};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind")]
pub enum TargetSpec {
    #[serde(rename = "auto")]
    #[default]
    Auto,
    #[serde(rename = "socketcan")]
    SocketCan { iface: String },
    #[serde(rename = "gs-usb-auto")]
    GsUsbAuto,
    #[serde(rename = "gs-usb-serial")]
    GsUsbSerial { serial: String },
    #[serde(rename = "gs-usb-bus-address")]
    GsUsbBusAddress { bus: u8, address: u8 },
    #[serde(rename = "daemon-udp")]
    DaemonUdp { addr: String },
    #[serde(rename = "daemon-uds")]
    DaemonUds { path: PathBuf },
}

impl TargetSpec {
    pub fn into_connection_target(self) -> ConnectionTarget {
        self.into()
    }
}

impl From<TargetSpec> for ConnectionTarget {
    fn from(value: TargetSpec) -> Self {
        match value {
            TargetSpec::Auto => ConnectionTarget::Auto,
            TargetSpec::SocketCan { iface } => ConnectionTarget::SocketCan { iface },
            TargetSpec::GsUsbAuto => ConnectionTarget::GsUsbAuto,
            TargetSpec::GsUsbSerial { serial } => ConnectionTarget::GsUsbSerial { serial },
            TargetSpec::GsUsbBusAddress { bus, address } => {
                ConnectionTarget::GsUsbBusAddress { bus, address }
            },
            TargetSpec::DaemonUdp { addr } => ConnectionTarget::DaemonUdp { addr },
            TargetSpec::DaemonUds { path } => ConnectionTarget::DaemonUds { path },
        }
    }
}

impl From<ConnectionTarget> for TargetSpec {
    fn from(value: ConnectionTarget) -> Self {
        match value {
            ConnectionTarget::Auto => TargetSpec::Auto,
            ConnectionTarget::SocketCan { iface } => TargetSpec::SocketCan { iface },
            ConnectionTarget::GsUsbAuto => TargetSpec::GsUsbAuto,
            ConnectionTarget::GsUsbSerial { serial } => TargetSpec::GsUsbSerial { serial },
            ConnectionTarget::GsUsbBusAddress { bus, address } => {
                TargetSpec::GsUsbBusAddress { bus, address }
            },
            ConnectionTarget::DaemonUdp { addr } => TargetSpec::DaemonUdp { addr },
            ConnectionTarget::DaemonUds { path } => TargetSpec::DaemonUds { path },
        }
    }
}

impl fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetSpec::Auto => write!(f, "auto"),
            TargetSpec::SocketCan { iface } => write!(f, "socketcan:{iface}"),
            TargetSpec::GsUsbAuto => write!(f, "gs-usb-auto"),
            TargetSpec::GsUsbSerial { serial } => write!(f, "gs-usb-serial:{serial}"),
            TargetSpec::GsUsbBusAddress { bus, address } => {
                write!(f, "gs-usb-bus-address:{bus}:{address}")
            },
            TargetSpec::DaemonUdp { addr } => write!(f, "daemon-udp:{addr}"),
            TargetSpec::DaemonUds { path } => write!(f, "daemon-uds:{}", path.display()),
        }
    }
}

impl FromStr for TargetSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto" {
            return Ok(Self::Auto);
        }
        if s == "gs-usb-auto" {
            return Ok(Self::GsUsbAuto);
        }

        let (kind, value) = s
            .split_once(':')
            .ok_or_else(|| "target spec must use '<kind>:<value>' format".to_string())?;

        match kind {
            "socketcan" => {
                if value.is_empty() {
                    return Err("socketcan target requires an interface name".to_string());
                }
                Ok(Self::SocketCan {
                    iface: value.to_string(),
                })
            },
            "gs-usb-serial" => {
                if value.is_empty() {
                    return Err("gs-usb-serial target requires a serial number".to_string());
                }
                Ok(Self::GsUsbSerial {
                    serial: value.to_string(),
                })
            },
            "gs-usb-bus-address" => {
                let (bus, address) = value
                    .split_once(':')
                    .ok_or_else(|| "gs-usb-bus-address must use '<bus>:<address>'".to_string())?;
                let bus = bus.parse::<u8>().map_err(|_| "invalid GS-USB bus number".to_string())?;
                let address =
                    address.parse::<u8>().map_err(|_| "invalid GS-USB address".to_string())?;
                Ok(Self::GsUsbBusAddress { bus, address })
            },
            "daemon-udp" => {
                if value.is_empty() {
                    return Err("daemon-udp target requires an address".to_string());
                }
                Ok(Self::DaemonUdp {
                    addr: value.to_string(),
                })
            },
            "daemon-uds" => {
                if value.is_empty() {
                    return Err("daemon-uds target requires a socket path".to_string());
                }
                Ok(Self::DaemonUds {
                    path: PathBuf::from(value),
                })
            },
            _ => Err(format!("unsupported target kind: {kind}")),
        }
    }
}

pub fn client_builder_for_target(target: &ConnectionTarget) -> ClientPiperBuilder {
    ClientPiperBuilder::new().target(target.clone())
}

pub fn driver_builder_for_target(target: &ConnectionTarget) -> DriverPiperBuilder {
    DriverPiperBuilder::new().target(target.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display_round_trip() {
        let cases = [
            "auto",
            "gs-usb-auto",
            "socketcan:vcan0",
            "gs-usb-serial:ABC123",
            "gs-usb-bus-address:1:8",
            "daemon-udp:127.0.0.1:18888",
            "daemon-uds:/tmp/gs_usb.sock",
        ];

        for case in cases {
            let parsed: TargetSpec = case.parse().unwrap();
            assert_eq!(parsed.to_string(), case);
        }
    }

    #[test]
    fn toml_serde_round_trip() {
        #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
        struct Wrapper {
            target: TargetSpec,
        }

        let wrappers = [
            Wrapper {
                target: TargetSpec::SocketCan {
                    iface: "vcan0".to_string(),
                },
            },
            Wrapper {
                target: TargetSpec::GsUsbBusAddress { bus: 2, address: 9 },
            },
            Wrapper {
                target: TargetSpec::DaemonUdp {
                    addr: "127.0.0.1:18888".to_string(),
                },
            },
            Wrapper {
                target: TargetSpec::DaemonUds {
                    path: PathBuf::from("/tmp/gs_usb.sock"),
                },
            },
        ];

        for wrapper in wrappers {
            let toml = toml::to_string(&wrapper).unwrap();
            let kind = match &wrapper.target {
                TargetSpec::Auto => "auto",
                TargetSpec::SocketCan { .. } => "socketcan",
                TargetSpec::GsUsbAuto => "gs-usb-auto",
                TargetSpec::GsUsbSerial { .. } => "gs-usb-serial",
                TargetSpec::GsUsbBusAddress { .. } => "gs-usb-bus-address",
                TargetSpec::DaemonUdp { .. } => "daemon-udp",
                TargetSpec::DaemonUds { .. } => "daemon-uds",
            };
            assert!(toml.contains(&format!("kind = \"{kind}\"")));

            let decoded: Wrapper = toml::from_str(&toml).unwrap();
            assert_eq!(decoded, wrapper);
        }
    }

    #[test]
    fn serde_kind_matches_cli_grammar() {
        #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
        struct Wrapper {
            target: TargetSpec,
        }

        let wrapper: Wrapper = toml::from_str(
            r#"
                [target]
                kind = "socketcan"
                iface = "can0"
            "#,
        )
        .unwrap();

        assert_eq!(
            wrapper.target,
            TargetSpec::SocketCan {
                iface: "can0".to_string()
            }
        );
    }
}
