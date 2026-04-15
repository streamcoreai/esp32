//! WiFi station (STA) connection helper for ESP32.

use anyhow::{bail, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::info;

pub fn connect<'d>(
    modem: Modem<'d>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Result<Box<EspWifi<'d>>> {
    let mut wifi = Box::new(EspWifi::new(modem, sysloop.clone(), Some(nvs))?);

    let auth = if password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };

    let mut ssid_buf = heapless::String::<32>::new();
    ssid_buf
        .push_str(ssid)
        .map_err(|_| anyhow::anyhow!("SSID too long (max 32)"))?;

    let mut pass_buf = heapless::String::<64>::new();
    pass_buf
        .push_str(password)
        .map_err(|_| anyhow::anyhow!("password too long (max 64)"))?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid_buf,
        password: pass_buf,
        auth_method: auth,
        ..Default::default()
    }))?;

    let mut blocking = BlockingWifi::wrap(&mut *wifi, sysloop)?;
    blocking.start()?;
    info!("WiFi started, scanning...");

    blocking.connect()?;
    info!("WiFi connected, waiting for IP...");

    blocking.wait_netif_up()?;

    let ip_info = blocking.wifi().sta_netif().get_ip_info()?;
    info!("WiFi got IP: {:?}", ip_info.ip);

    if ip_info.ip.is_unspecified() {
        bail!("failed to obtain IP address");
    }

    Ok(wifi)
}
