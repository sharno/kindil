//! HTTP edge: one request, one freshly rendered frame.

use anyhow::Context;
use chrono::{DateTime, Utc};
use tiny_http::{Header, Response, Server};

use crate::config::Config;
use crate::layout::Frame;
use crate::prayer::{self, DayTimes};
use crate::render::Renderer;
use crate::sources::Sources;

pub struct App {
    sources: Sources,
    renderer: Renderer,
}

impl App {
    pub fn new(config: &Config) -> Self {
        Self {
            sources: Sources::new(config),
            renderer: Renderer::new(config.display.contrast),
        }
    }

    pub fn frame(&mut self, config: &Config, now: DateTime<Utc>) -> anyhow::Result<Vec<u8>> {
        self.sources.refresh(config, now);
        let date = prayer::local_date(&config.location, now);
        let prayers = DayTimes::compute(&config.location, &config.prayer, date)?;
        self.renderer.png(&Frame {
            now,
            config,
            prayers: &prayers,
            weather: self.sources.weather(),
            radar: self.sources.radar(),
        })
    }
}

const PREVIEW: &str = r#"<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="60"><title>miqat</title><body style="margin:0;background:#888;display:grid;place-items:center;min-height:100vh"><img src="/dash.png" width="758" height="1024" style="max-width:100%;height:auto;box-shadow:0 0 20px #0006">"#;

pub fn run(config: &Config) -> anyhow::Result<()> {
    let server = Server::http(config.server.listen)
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| format!("listening on {}", config.server.listen))?;
    eprintln!("serving on http://{}/dash.png", config.server.listen);
    let mut app = App::new(config);
    // Requests are handled one at a time: a single Kindle polls once a minute.
    server.incoming_requests().for_each(|request| {
        let response = match request.url().split('?').next() {
            Some("/dash.png") => match app.frame(config, Utc::now()) {
                Ok(png) => {
                    Response::from_data(png).with_header(header("Content-Type", "image/png"))
                }
                Err(e) => {
                    eprintln!("render: {e:#}");
                    Response::from_string(format!("{e:#}")).with_status_code(500)
                }
            },
            Some("/") => Response::from_string(PREVIEW)
                .with_header(header("Content-Type", "text/html; charset=utf-8")),
            Some("/healthz") => Response::from_string("ok"),
            _ => Response::from_string("not found").with_status_code(404),
        };
        if let Err(e) = request.respond(response.with_header(header("Cache-Control", "no-store"))) {
            eprintln!("respond: {e}");
        }
    });
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name, value).expect("static header is valid ASCII")
}
