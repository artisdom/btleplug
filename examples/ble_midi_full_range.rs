// See the "macOS permissions note" in README.md before running this on macOS
// Big Sur or later.

use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral, ScanFilter, WriteType};
use btleplug::platform::Manager;
use btleplug::Error as BtleplugError;
use futures::stream::StreamExt;
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

/// Only devices whose name contains this string will be tried.
const PERIPHERAL_NAME_MATCH_FILTER: &str = "MIDI";
/// Standard BLE MIDI service UUID.
const MIDI_SERVICE_UUID: Uuid = Uuid::from_u128(0x03b80e5a_ede8_4b33_a751_6ce34ec4c700);
/// Standard BLE MIDI characteristic UUID.
const MIDI_CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0x7772e5db_3868_4112_a1a9_f2669d106bf3);

const FIRST_PIANO_NOTE: u8 = 21; // A0
const LAST_PIANO_NOTE: u8 = 108; // C8
const NOTE_ON_VELOCITY: u8 = 0x64;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pretty_env_logger::init();

    let manager = Manager::new().await?;
    let adapter_list = manager.adapters().await?;
    if adapter_list.is_empty() {
        eprintln!("No Bluetooth adapters found");
    }

    for adapter in adapter_list.iter() {
        println!("Starting scan...");
        adapter
            .start_scan(ScanFilter::default())
            .await
            .expect("Can't scan BLE adapter for connected devices...");
        time::sleep(Duration::from_secs(5)).await;
        let peripherals = adapter.peripherals().await?;

        if peripherals.is_empty() {
            eprintln!("->>> BLE peripheral devices were not found, sorry. Exiting...");
        } else {
            for peripheral in peripherals.iter() {
                let maybe_properties = peripheral.properties().await?;
                let is_connected = peripheral.is_connected().await?;
                let local_name = maybe_properties
                    .as_ref()
                    .and_then(|p| p.local_name.clone())
                    .unwrap_or_else(|| String::from("(peripheral name unknown)"));

                println!(
                    "Peripheral {:?} (connected: {:?})",
                    &local_name, is_connected
                );

                if !local_name.contains(PERIPHERAL_NAME_MATCH_FILTER) {
                    println!("Skipping peripheral {:?}, filter mismatch", &local_name);
                    continue;
                }

                if let Some(properties) = maybe_properties.as_ref() {
                    if !properties.services.is_empty()
                        && !properties.services.contains(&MIDI_SERVICE_UUID)
                    {
                        println!(
                            "Peripheral {:?} does not advertise BLE MIDI service, skipping",
                            &local_name
                        );
                        continue;
                    }
                }

                println!("Found candidate peripheral {:?}...", &local_name);

                if !is_connected {
                    if let Err(err) = peripheral.connect().await {
                        eprintln!("Error connecting to peripheral {:?}: {}", local_name, err);
                        continue;
                    }
                }

                let is_connected = peripheral.is_connected().await?;
                println!("Now connected ({:?}) to {:?}", is_connected, &local_name);
                if !is_connected {
                    continue;
                }

                println!("Discovering services for {:?}...", &local_name);
                peripheral.discover_services().await?;

                let midi_characteristic = match find_midi_characteristic(peripheral) {
                    Some(characteristic) => characteristic,
                    None => {
                        eprintln!(
                            "Peripheral {:?} does not expose BLE MIDI characteristic",
                            &local_name
                        );
                        peripheral.disconnect().await?;
                        continue;
                    }
                };

                println!(
                    "Subscribing to BLE MIDI characteristic {:?}",
                    midi_characteristic.uuid
                );
                println!(
                    "Characteristic properties: {:?}",
                    midi_characteristic.properties
                );
                peripheral.subscribe(&midi_characteristic).await?;

                let mut notification_stream = peripheral.notifications().await?;
                let name_for_task = local_name.clone();
                let notification_task = tokio::spawn(async move {
                    let mut count = 0u32;
                    while let Some(data) = notification_stream.next().await {
                        count += 1;
                        println!(
                            "Notification #{:?} from {:?} [{:?}]: {:?}",
                            count, name_for_task, data.uuid, data.value
                        );
                    }
                });

                println!(
                    "Sending full piano range notes ({}-{})",
                    FIRST_PIANO_NOTE, LAST_PIANO_NOTE
                );
                for note in FIRST_PIANO_NOTE..=LAST_PIANO_NOTE {
                    let note_on = ble_midi_message(true, note, NOTE_ON_VELOCITY);
                    println!("Note ON  -> {}", note);
                    write_with_fallback(peripheral, &midi_characteristic, &note_on).await?;
                    time::sleep(Duration::from_millis(80)).await;

                    let note_off = ble_midi_message(false, note, 0x00);
                    println!("Note OFF -> {}", note);
                    write_with_fallback(peripheral, &midi_characteristic, &note_off).await?;
                    time::sleep(Duration::from_millis(20)).await;
                }

                println!("Finished sending notes, allowing notifications to drain...");
                time::sleep(Duration::from_secs(1)).await;

                println!("Unsubscribing from BLE MIDI characteristic");
                peripheral.unsubscribe(&midi_characteristic).await?;

                println!("Disconnecting from peripheral {:?}...", &local_name);
                peripheral.disconnect().await?;

                notification_task.abort();
            }
        }
    }

    Ok(())
}

fn find_midi_characteristic(peripheral: &impl Peripheral) -> Option<btleplug::api::Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == MIDI_CHARACTERISTIC_UUID)
        .filter(|c| {
            c.properties.contains(CharPropFlags::NOTIFY)
                && (c.properties.contains(CharPropFlags::WRITE)
                    || c.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE))
        })
}

fn ble_midi_message(is_note_on: bool, note: u8, velocity: u8) -> [u8; 4] {
    let status = if is_note_on { 0x90 } else { 0x80 };
    [0x80, status, note, velocity]
}

async fn write_with_fallback(
    peripheral: &impl Peripheral,
    characteristic: &btleplug::api::Characteristic,
    data: &[u8],
) -> btleplug::Result<()> {
    let supports_without_response = characteristic
        .properties
        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE);
    let supports_with_response = characteristic.properties.contains(CharPropFlags::WRITE);

    if supports_without_response {
        match peripheral
            .write(characteristic, data, WriteType::WithoutResponse)
            .await
        {
            Ok(()) => return Ok(()),
            Err(err) => {
                if should_try_with_response(&err, supports_with_response) {
                    eprintln!(
                        "Write without response failed ({}); retrying with response",
                        err
                    );
                } else {
                    return Err(augment_write_error(err));
                }
            }
        }
    }

    if supports_with_response || supports_without_response {
        return peripheral
            .write(characteristic, data, WriteType::WithResponse)
            .await
            .map_err(augment_write_error);
    }

    Err(btleplug::Error::NotSupported(
        "BLE MIDI characteristic is not writable".into(),
    ))
}

fn should_try_with_response(err: &btleplug::Error, supports_with_response: bool) -> bool {
    supports_with_response || is_not_authorized_error(err)
}

fn is_not_authorized_error(err: &btleplug::Error) -> bool {
    let text = err.to_string();
    text.contains("Not Authorized") || text.contains("Not authorized")
}

fn augment_write_error(err: btleplug::Error) -> btleplug::Error {
    if is_not_authorized_error(&err) {
        BtleplugError::Other(
            format!(
                "Operation not authorized while writing to BLE MIDI characteristic. \
                 Many devices require pairing/bonding before they accept MIDI writes. \
                 Pair the device (for example via `bluetoothctl pair/trust/connect`) and retry. \
                 Original error: {}",
                err
            )
            .into(),
        )
    } else {
        err
    }
}
