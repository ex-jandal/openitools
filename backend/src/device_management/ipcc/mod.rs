pub mod prep_file;
pub mod query;

use query::download_bundle;
use regex::Regex;
use rsmobiledevice::{
    device_syslog::{filters::FilterPart, LogFilter},
    RecursiveFind,
};
use std::{io::Cursor, sync::Arc};
use tauri::Emitter;

#[tauri::command]
pub async fn install_ipcc(
    window: tauri::Window,
    device_model: String,
    ios_version: String,
    bundle: String,
) {
    let device_client_res = rsmobiledevice::device::DeviceClient::new().and_then(|client| {
        client
            .get_first_device()
            .ok_or(rsmobiledevice::errors::DeviceClientError::DeviceNotFound)
    });

    match device_client_res {
        Ok(device_client) => {
            let device_info = device_client.get_device_info();

            let connected_model = device_info.get_product_type().unwrap_or_default();

            let connected_ios_ver = device_info.get_product_version().unwrap_or_default();

            // if the sent model and ios version doesn't match the connected ones, fail and
            // return
            if device_model != connected_model || ios_version != connected_ios_ver {
                log::info!(
                        "Model or iOS version mismatch: expected {connected_model}:{connected_ios_ver}, got {device_model}:{ios_version}",
                    );
                window.emit("carrier_bundle_install_status", false).ok();
                return;
            }

            let window = Arc::new(window);
            let window_clone = Arc::clone(&window);

            let install_client = device_client.get_device_installer();

            let tar_file_bytes = download_bundle(&device_model, &ios_version, &bundle).await;
            let mut zip_file_bytes =
                Cursor::new(prep_file::repack_tar_to_zip(&tar_file_bytes).await);

            if let Err(e) = install_client.install_from_reader_with_callback(
                &mut zip_file_bytes,
                None,
                move |command, status| {
                    println!("{command:#?}");
                    println!("{status:#?}");
                    // once we recursivly find the `Status` key and it's value is `Complete`
                    // meaning the installation is successful
                    if status.rfind("Status").is_some_and(|s| &s == "Complete") {
                        window_clone
                            .emit("carrier_bundle_install_status", true)
                            .ok();
                    }
                },
            ) {
                log::error!("Installation failed: {e}");
                window.emit("carrier_bundle_install_status", true).ok();
            } else {
                log::info!("IPCC installation started");
            }
        }
        Err(client_error) => {
            log::error!("Failed to initialize device client: {client_error}");
            window.emit("carrier_bundle_install_status", false).ok();
        }
    }
}

#[tauri::command]
pub fn check_installing_succeed(window: tauri::Window) {
    let device_client_res = rsmobiledevice::device::DeviceClient::new().and_then(|client| {
        client
            .get_first_device()
            .ok_or(rsmobiledevice::errors::DeviceClientError::DeviceNotFound)
    });

    match device_client_res {
        Ok(device_client) => {
            // we don't want to drop the syslog client after the function ends
            // might change later
            // TODO: maybe use async instead of a thread
            std::thread::spawn(|| {
                let mut syslog_client = device_client.get_device_syslog();

                match Regex::new(r"/\b\w*SIM is Ready\w*\b/i") {
                    Ok(re) => {
                        // usually there will be a message about the sim being ready in the logs if the carrier
                        // bundle installation is good
                        syslog_client.set_filter(LogFilter::OneShot(re), FilterPart::All);
                    }
                    Err(e) => {
                        log::error!("Failed to create a new regex, error: {e}");
                        window.emit("installation_succeed_status", false).ok();
                        return;
                    }
                }

                let window = Arc::new(window);

                let window_1 = Arc::clone(&window);
                let window_2 = Arc::clone(&window);

                // the first callback should be called once the filter succeed to be found and it will
                // stop because we specifed the OneShot, which basically stops the logging if the
                // filter applied
                //
                // if not and it exceeded the timeout, the second callback would get called, thus
                // triggering the false payload
                if let Err(e) = syslog_client.log_to_custom_with_timeout_or_else(
                    move |_| {
                        log::info!("SIM ready detected");
                        window_1.emit("installation_succeed_status", true).ok();
                    },
                    std::time::Duration::from_secs(40),
                    move || {
                        log::warn!("SIM ready not detected within 40s");
                        window_2.emit("installation_succeed_status", false).ok();
                    },
                ) {
                    log::error!("Syslog monitoring failed: {e}");
                    window.emit("installation_succeed_status", false).ok();
                }
            });
        }

        Err(e) => {
            log::error!("Failed to initialize device client: {e}");
            window.emit("installation_succeed_status", false).ok();
        }
    }
}
