use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use archipelago_rs::{Connection, ConnectionOptions, Event, ItemHandling};
use bb_archipelago::backend::{
    BloodborneBackend, FileBackend, GoodsGrant, GrantProgress, MockBackend,
};
use bb_archipelago::bridge::FileBridge;
use bb_archipelago::client_loop::{ClientLoop, IncomingItem};
use bb_archipelago::config::RuntimeConfig;
use bb_archipelago::event_flags::LiveEventFlags;
use bb_archipelago::ledger::ReceiveLedger;

enum Backend {
    Live(FileBackend),
    Mock(MockBackend),
}

impl BloodborneBackend for Backend {
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        match self {
            Self::Live(backend) => backend.read_event_flag(event_flag),
            Self::Mock(backend) => backend.read_event_flag(event_flag),
        }
    }

    fn grant_category4_goods(&mut self, grant: &GoodsGrant) -> Result<GrantProgress> {
        match self {
            Self::Live(backend) => backend.grant_category4_goods(grant),
            Self::Mock(backend) => backend.grant_category4_goods(grant),
        }
    }
}

struct Arguments {
    server: String,
    slot: String,
    config: PathBuf,
    ledger: PathBuf,
    password: Option<String>,
    mock: bool,
}

fn arguments() -> Result<Arguments> {
    let mut args = env::args().skip(1);
    let Some(server) = args.next() else {
        bail!("usage: bb-ap-client SERVER SLOT CONFIG LEDGER [PASSWORD] [--mock]")
    };
    let slot = args.next().context("missing SLOT")?;
    let config = args.next().context("missing CONFIG")?.into();
    let ledger = args.next().context("missing LEDGER")?.into();
    let mut password = None;
    let mut mock = false;
    for argument in args {
        if argument == "--mock" {
            mock = true;
        } else if password.replace(argument).is_some() {
            bail!("only one password may be supplied");
        }
    }
    Ok(Arguments {
        server,
        slot,
        config,
        ledger,
        password,
        mock,
    })
}

fn main() -> Result<()> {
    let args = arguments()?;
    let config = RuntimeConfig::load(&args.config)?;
    let backend = if args.mock {
        let mut backend = MockBackend::default();
        backend
            .set_flags
            .extend(config.mock_set_flags.iter().copied());
        Backend::Mock(backend)
    } else {
        let shad_log = config
            .shad_log
            .as_deref()
            .context("live mode requires shad_log in the runtime config")?;
        let event_flags = LiveEventFlags::attach(shad_log)?;
        let attachment = event_flags.info();
        eprintln!(
            "Bloodborne AP client {} | CUSA03173 01.09 | shad PID {} | eboot 0x{:X} | direct flag backend ready",
            env!("CARGO_PKG_VERSION"),
            attachment.process_id,
            attachment.eboot_base
        );
        Backend::Live(FileBackend::new(
            FileBridge::new(&config.bridge_root),
            event_flags,
        ))
    };
    let ledger = ReceiveLedger::load(&args.ledger)?;
    let mut backend = Some(backend);
    let mut ledger = Some(ledger);
    let mut options = ConnectionOptions::new().receive_items(ItemHandling::OtherWorlds {
        own_world: true,
        starting_inventory: true,
    });
    if let Some(password) = args.password {
        options = options.password(password);
    }
    let mut connection =
        Connection::<json::Value>::new(args.server, args.slot.clone(), Some("Bloodborne"), options);
    let mut runtime = None;
    let mut last_location_error: Option<(String, Instant)> = None;

    loop {
        let mut connected_now = false;
        for event in connection.update() {
            match event {
                Event::Connected => {
                    connected_now = true;
                    eprintln!("Connected to Archipelago.");
                }
                Event::Print(message) => eprintln!("{message}"),
                Event::Error(error) => eprintln!("Archipelago error: {error}"),
                _ => {}
            }
        }
        if connected_now && let Some(client) = connection.client_mut() {
            client.sync()?;
        }

        if runtime.is_none()
            && let Some(client) = connection.client()
        {
            runtime = Some(ClientLoop::new(
                backend.take().context("backend was already initialized")?,
                config.clone(),
                ledger.take().context("ledger was already initialized")?,
                args.ledger.clone(),
                client.seed_name(),
                args.slot.clone(),
            ));
        }

        if let (Some(runtime), Some(client)) = (runtime.as_mut(), connection.client_mut()) {
            let checked = client
                .checked_locations()
                .map(|location| location.id())
                .collect::<HashSet<_>>();
            match runtime.poll_locations(&checked) {
                Ok(newly_checked) => {
                    if last_location_error.take().is_some() {
                        eprintln!("Bloodborne location polling recovered.");
                    }
                    if !newly_checked.is_empty() {
                        client.mark_checked(newly_checked.iter().copied())?;
                        eprintln!("Sent location checks: {newly_checked:?}");
                    }
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    let report = last_location_error.as_ref().is_none_or(|(previous, when)| {
                        previous != &message || when.elapsed() >= Duration::from_secs(10)
                    });
                    if report {
                        eprintln!("Bloodborne location polling unavailable: {message}");
                        last_location_error = Some((message, Instant::now()));
                    }
                }
            }

            let received = client
                .received_items()
                .iter()
                .map(|item| IncomingItem {
                    index: item.index() as u64,
                    ap_item_id: item.item().id(),
                })
                .collect::<Vec<_>>();
            if runtime.poll_items(&received)? {
                eprintln!("Acknowledged one received item.");
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}
