// See the "macOS permissions note" in README.md before running this on macOS
// Big Sur or later.

use btleplug::api::{
    bleuuid::uuid_from_u16, Central, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use rand::{thread_rng, Rng};
use std::error::Error;
use std::time::Duration;
use uuid::Uuid;

const LIGHT_CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0x7772e5db_3868_4112_a1a9_f2669d106bf3);
use tokio::time;

async fn find_light(central: &Adapter) -> Option<Peripheral> {
    for p in central.peripherals().await.unwrap() {
        if p.properties()
            .await
            .unwrap()
            .unwrap()
            .local_name
            .iter()
            .any(|name| name.contains("GZUT-MIDI"))
        {
            return Some(p);
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let manager = Manager::new().await.unwrap();

    // get the first bluetooth adapter
    let central = manager
        .adapters()
        .await
        .expect("Unable to fetch adapter list.")
        .into_iter()
        .nth(0)
        .expect("Unable to find adapters.");

    // start scanning for devices
    central.start_scan(ScanFilter::default()).await?;
    // instead of waiting, you can use central.events() to get a stream which will
    // notify you of new devices, for an example of that see examples/event_driven_discovery.rs
    time::sleep(Duration::from_secs(10)).await;

    // find the device we're interested in
    let light = find_light(&central).await.expect("No lights found");

    println!("Starting connecting");
    // connect to the device
    light.connect().await?;

    println!("Discover services");
    // discover services and characteristics
    light.discover_services().await?;

    // find the characteristic we want
    let chars = light.characteristics();
    let cmd_char = chars
        .iter()
        .find(|c| c.uuid == LIGHT_CHARACTERISTIC_UUID)
        .expect("Unable to find characterics");

    // dance party
    let mut rng = thread_rng();
    let mut i = 0;
    let led_start = 21;
    let led_end = 108;

    for i in led_start..=led_end {
        let led_cmd = vec![0x80, 0x80, 0x90, i, 0x02];
        light
            .write(&cmd_char, &led_cmd, WriteType::WithoutResponse)
            .await?;
        time::sleep(Duration::from_millis(50)).await;
    }

    for i in led_start..=led_end {
        let led_cmd = vec![0x80, 0x80, 0x90, i, 0x00];
        light
            .write(&cmd_char, &led_cmd, WriteType::WithoutResponse)
            .await?;
        time::sleep(Duration::from_millis(50)).await;
    }

    Ok(())
}
