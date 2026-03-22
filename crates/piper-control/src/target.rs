use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_driver::{ConnectionTarget, PiperBuilder as DriverPiperBuilder};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind")]
pub enum TargetSpec {
    #[serde(rename = "auto-strict")]
    #[default]
    AutoStrict,
    #[serde(rename = "auto-any")]
    AutoAny,
    #[serde(rename = "socketcan")]
    SocketCan { iface: String },
    #[serde(rename = "gs-usb-auto")]
    GsUsbAuto,
    #[serde(rename = "gs-usb-serial")]
    GsUsbSerial { serial: String },
    #[serde(rename = "gs-usb-bus-address")]
    GsUsbBusAddress { bus: u8, address: u8 },
}

impl TargetSpec {
    pub fn into_connection_target(self) -> ConnectionTarget {
        self.into()
    }
}

impl From<TargetSpec> for ConnectionTarget {
    fn from(value: TargetSpec) -> Self {
        match value {
            TargetSpec::AutoStrict => ConnectionTarget::AutoStrict,
            TargetSpec::AutoAny => ConnectionTarget::AutoAny,
            TargetSpec::SocketCan { iface } => ConnectionTarget::SocketCan { iface },
            TargetSpec::GsUsbAuto => ConnectionTarget::GsUsbAuto,
            TargetSpec::GsUsbSerial { serial } => ConnectionTarget::GsUsbSerial { serial },
            TargetSpec::GsUsbBusAddress { bus, address } => {
                ConnectionTarget::GsUsbBusAddress { bus, address }
            },
        }
    }
}

impl From<ConnectionTarget> for TargetSpec {
    fn from(value: ConnectionTarget) -> Self {
        match value {
            ConnectionTarget::AutoStrict => TargetSpec::AutoStrict,
            ConnectionTarget::AutoAny => TargetSpec::AutoAny,
            ConnectionTarget::SocketCan { iface } => TargetSpec::SocketCan { iface },
            ConnectionTarget::GsUsbAuto => TargetSpec::GsUsbAuto,
            ConnectionTarget::GsUsbSerial { serial } => TargetSpec::GsUsbSerial { serial },
            ConnectionTarget::GsUsbBusAddress { bus, address } => {
                TargetSpec::GsUsbBusAddress { bus, address }
            },
        }
    }
}

impl fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetSpec::AutoStrict => write!(f, "auto-strict"),
            TargetSpec::AutoAny => write!(f, "auto-any"),
            TargetSpec::SocketCan { iface } => write!(f, "socketcan:{iface}"),
            TargetSpec::GsUsbAuto => write!(f, "gs-usb-auto"),
            TargetSpec::GsUsbSerial { serial } => write!(f, "gs-usb-serial:{serial}"),
            TargetSpec::GsUsbBusAddress { bus, address } => {
                write!(f, "gs-usb-bus-address:{bus}:{address}")
            },
        }
    }
}

impl FromStr for TargetSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto-strict" {
            return Ok(Self::AutoStrict);
        }
        if s == "auto-any" {
            return Ok(Self::AutoAny);
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
            "auto-strict",
            "auto-any",
            "gs-usb-auto",
            "socketcan:vcan0",
            "gs-usb-serial:ABC123",
            "gs-usb-bus-address:1:8",
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
        ];

        for wrapper in wrappers {
            let toml = toml::to_string(&wrapper).unwrap();
            let kind = match &wrapper.target {
                TargetSpec::AutoStrict => "auto-strict",
                TargetSpec::AutoAny => "auto-any",
                TargetSpec::SocketCan { .. } => "socketcan",
                TargetSpec::GsUsbAuto => "gs-usb-auto",
                TargetSpec::GsUsbSerial { .. } => "gs-usb-serial",
                TargetSpec::GsUsbBusAddress { .. } => "gs-usb-bus-address",
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

    #[test]
    fn daemon_targets_are_rejected() {
        assert!("daemon-udp:127.0.0.1:18888".parse::<TargetSpec>().is_err());
        assert!("daemon-uds:/tmp/gs_usb.sock".parse::<TargetSpec>().is_err());
    }
}
