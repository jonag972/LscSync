use std::future::Future;
use std::io::Write as _;
use std::time::{Duration, Instant};

use lscsync_core::{Hsv, Profile, Settings};
use lscsync_engine::tuya::{Lsc, LscConfig};
use lscsync_engine::worker::{Engine, Target};
use lscsync_platform::{legacy_settings_path, profile_path};

const RED: Hsv = Hsv::new(0.0, 1.0, 1.0);
const GREEN: Hsv = Hsv::new(1.0 / 3.0, 1.0, 1.0);
const BLUE: Hsv = Hsv::new(2.0 / 3.0, 1.0, 1.0);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("status") => block_on(status())?,
        Some("white") => block_on(white())?,
        Some("pattern") => block_on(pattern())?,
        Some("preview") => preview()?,
        Some("run") => run(parse_seconds(args.get(1)))?,
        Some("audio") => audio(parse_seconds(args.get(1)))?,
        Some("scan") => block_on(scan())?,
        Some("login") => login(args.get(1).map(String::as_str) == Some("--save"))?,
        Some("help" | "--help" | "-h") => usage(),
        _ => {
            println!("LscSync {} (Rust)", env!("CARGO_PKG_VERSION"));
            println!("profile: {}", profile_path().display());
            usage();
        }
    }

    Ok(())
}

fn usage() {
    println!("usage:");
    println!("  lscsync-cli status        checks the connection to the panels");
    println!("  lscsync-cli white         sets the panels back to white");
    println!("  lscsync-cli pattern       red / green / blue on the 3 zones, then white");
    println!("  lscsync-cli preview       prints the captured colors, sends nothing");
    println!("  lscsync-cli run [sec]     live sync of the synced devices (15 s by default)");
    println!("  lscsync-cli audio [sec]   checks the sound capture, no panels needed");
    println!("  lscsync-cli scan          lists the Tuya devices on the local network");
    println!("  lscsync-cli login [--save] lists the account's devices via QR login");
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime tokio")
        .block_on(future)
}

fn parse_seconds(value: Option<&String>) -> u64 {
    value.and_then(|value| value.parse().ok()).unwrap_or(15)
}

/// Synced device and settings, from the profile.
fn load() -> Result<(LscConfig, Settings), Box<dyn std::error::Error>> {
    lscsync_platform::migrate_legacy_data()?;
    let profile = Profile::load_or_migrate(&profile_path(), &legacy_settings_path())?;
    let device = profile
        .sync_target()
        .ok_or("no device configured in the profile")?;
    Ok((LscConfig::from(device), profile.settings))
}

async fn status() -> Result<(), Box<dyn std::error::Error>> {
    let (device, _) = load()?;
    let lsc = Lsc::connect(&device, Duration::from_secs(10)).await?;
    println!("connected to {} ({})", device.ip, device.device_id);

    let mut events = lsc.listener();
    lsc.query().await?;
    match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
        Ok(Some(rustuya::tokio::Event::Frame(frame))) => {
            println!("status: {}", String::from_utf8_lossy(&frame.payload));
        }
        Ok(Some(rustuya::tokio::Event::Lagged(n))) => println!("status: {n} frames lost"),
        Ok(None) => println!("status: connection closed"),
        Err(_) => println!("status: no answer within 5 s"),
    }

    lsc.close().await;
    Ok(())
}

async fn white() -> Result<(), Box<dyn std::error::Error>> {
    let (device, _) = load()?;
    let lsc = Lsc::connect(&device, Duration::from_secs(10)).await?;
    lsc.restore_white().await?;
    lsc.close().await;
    println!("back to white sent");
    Ok(())
}

/// Synced devices, as engine targets, and the settings.
fn load_targets() -> Result<(Vec<Target>, Settings), Box<dyn std::error::Error>> {
    lscsync_platform::migrate_legacy_data()?;
    let profile = Profile::load_or_migrate(&profile_path(), &legacy_settings_path())?;
    let targets: Vec<Target> = profile
        .sync_targets()
        .map(|device| Target::new(device, profile.settings.capture_height))
        .collect();
    if targets.is_empty() {
        return Err("no synced device in the profile".into());
    }
    Ok((targets, profile.settings))
}

/// The color each zone of each synced device would get from the screen.
fn preview() -> Result<(), Box<dyn std::error::Error>> {
    let (targets, settings) = load_targets()?;
    let image = lscsync_engine::capture::ScreenCapture::new()?.capture()?;
    for target in &targets {
        println!("{}:", target.name);
        for (index, zone) in target.zones.iter().enumerate() {
            let color = lscsync_core::color::to_lamp(
                lscsync_core::color::dominant_color(&image.region(*zone)),
                settings.boost_sat,
                settings.lum_max as f32,
            );
            println!("  zone {}: {color:?}", index + 1);
        }
    }
    Ok(())
}

async fn pattern() -> Result<(), Box<dyn std::error::Error>> {
    let (device, settings) = load()?;
    let lsc = Lsc::connect(&device, Duration::from_secs(10)).await?;
    println!("connected to {} ({})", device.ip, device.device_id);
    println!("pattern: red left, green middle, blue right (3 s)");

    lsc.send_zones(&[RED, GREEN, BLUE], settings.invert_zones)
        .await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    if settings.restore_white {
        println!("back to white");
        lsc.restore_white().await?;
    }

    lsc.close().await;
    println!("done");
    Ok(())
}

fn audio(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut capture = lscsync_engine::audio::PlatformAudioCapture::new()?;
    capture.start()?;
    println!("capturing sound for {seconds} s…");

    let start = std::time::Instant::now();
    let (mut blocks, mut silent, mut peak) = (0_u32, 0_u32, 0.0_f32);
    while start.elapsed() < Duration::from_secs(seconds) {
        match capture.read_block_timeout(Duration::from_millis(100))? {
            Some(block) => {
                blocks += 1;
                peak = block.iter().fold(peak, |peak, s| peak.max(s.abs()));
            }
            None => silent += 1,
        }
    }

    capture.stop();
    println!("{blocks} blocks received, {silent} silent waits, peak {peak:.3}");
    Ok(())
}

async fn scan() -> Result<(), Box<dyn std::error::Error>> {
    println!("listening for 5 s…");
    let found = lscsync_engine::tuya::scan_local_network(Duration::from_secs(5)).await?;
    println!("{} device(s) on the local network:", found.len());
    for (id, ip) in found {
        println!("- {id}  {ip}");
    }
    Ok(())
}

/// QR login with the LSC app; `--save` merges the devices into the profile.
fn login(save: bool) -> Result<(), Box<dyn std::error::Error>> {
    print!("LSC app user code (Me > Settings > Account and Security > User Code): ");
    std::io::stdout().flush()?;
    let mut user_code = String::new();
    std::io::stdin().read_line(&mut user_code)?;

    let login = lscsync_cloud::QrLogin::start(user_code.trim())?;
    let qr = qrcode::QrCode::new(login.qr_content())?;
    // Inverted colors: readable on dark terminals.
    let image = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .build();
    println!("{image}");
    println!("Scan it with the LSC app's scanner, then confirm. Waiting up to 3 min…");

    let deadline = Instant::now() + Duration::from_secs(180);
    let session = loop {
        if let Some(session) = login.poll()? {
            break session;
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for the scan".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    };

    let devices = session.devices()?;
    println!("\n{} device(s):", devices.len());
    for device in &devices {
        let key = &device.local_key;
        let masked = match key.len() {
            0..=4 => "(short)".to_owned(),
            len => format!("{}…{} ({len} chars)", &key[..2], &key[len - 2..]),
        };
        println!(
            "- {} [{} · {}] online: {}\n  id: {}  key: {masked}",
            device.name, device.product_name, device.category, device.online, device.id
        );
    }

    if save {
        lscsync_platform::migrate_legacy_data()?;
        let mut profile = Profile::load_or_migrate(&profile_path(), &legacy_settings_path())?;
        profile.merge_devices(devices.into_iter().map(Into::into).collect());
        for (id, ip) in block_on(lscsync_engine::tuya::scan_local_network(
            Duration::from_secs(4),
        ))? {
            profile.set_ip(&id, ip);
        }
        profile.save(&profile_path())?;
        println!("saved to {}", profile_path().display());
    }
    Ok(())
}

fn run(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let (targets, settings) = load_targets()?;
    let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    println!(
        "{:?} for {seconds} s on {}",
        settings.mode,
        names.join(", ")
    );

    let mut engine = Engine::new(targets, settings, None)?;
    let stats = engine.run(Duration::from_secs(seconds))?;
    engine.close();
    println!(
        "{} frames computed in {:.1} s",
        stats.frames,
        stats.elapsed.as_secs_f32()
    );
    println!("done");
    Ok(())
}
