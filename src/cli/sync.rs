use crate::client::endpoint::catalog_sync::{self, Source};

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let result = match args {
        [command] if command == "serve" => {
            crate::client::sync_background::serve()?;
            return Ok(0);
        }
        [command] if command == "auto" => {
            catalog_sync::enable_automatic().map_err(std::io::Error::other)?;
            crate::client::sync_background::start()?;
            return Ok(0);
        }
        [command, action] if command == "background" => {
            match action.as_str() {
                "start" => crate::client::sync_background::start()?,
                "stop" => crate::client::sync_background::stop()?,
                "status" => {}
                _ => {
                    return Err(std::io::Error::other(
                        "usage: herdr sync background <start|stop|status>",
                    ))
                }
            }
            println!("{}", crate::client::sync_background::status());
            return Ok(0);
        }
        [command] if command == "check-v1" => {
            println!("herdr-catalog-sync-v1");
            return Ok(0);
        }
        [command, target] if command == "use" => crate::remote::prepare_catalog_host(target)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                catalog_sync::configure(Source::Ssh {
                    target: target.clone(),
                })
            }),

        [command] if command == "host" => catalog_sync::configure(Source::Local),
        [command, target] if command == "join" => catalog_sync::configure(Source::Ssh {
            target: target.clone(),
        }),
        [command] if command == "leave" => {
            catalog_sync::leave().and_then(|()| crate::client::direct_sync::disable())
        }
        [command]
            if command == "now"
                && catalog_sync::status().map_err(std::io::Error::other)?["source"]["kind"]
                    == "direct" =>
        {
            crate::client::direct_sync::submit(crate::client::direct_sync::Action::SyncNow)
        }
        [command] if command == "now" => catalog_sync::sync_once().and_then(|enabled| {
            if enabled {
                Ok(())
            } else {
                Err(
                    "sync is not configured; use `herdr sync host` or `herdr sync join HOST`"
                        .into(),
                )
            }
        }),
        [command] if command == "exchange-v1" => {
            return catalog_sync::exchange_stdio()
                .map(|()| 0)
                .map_err(std::io::Error::other)
        }
        [command] if command == "status" => {
            println!("{}", status()?);
            return Ok(0);
        }
        [command, flag] if command == "status" && flag == "--json" => {
            println!("{}", status()?);
            return Ok(0);
        }
        _ => {
            return Err(std::io::Error::other(
                "usage: herdr sync <host|join SSH_TARGET|leave|status|now>",
            ))
        }
    };
    result.map_err(std::io::Error::other)?;
    println!("{}", catalog_sync::status().map_err(std::io::Error::other)?);
    Ok(0)
}

fn status() -> std::io::Result<serde_json::Value> {
    let mut value = catalog_sync::status().map_err(std::io::Error::other)?;
    value["background"] = crate::client::sync_background::status();
    Ok(value)
}
