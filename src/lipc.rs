//! Amazon's IPC on the stock OS: `lipc-get-prop SERVICE PROPERTY` and friends.
use anyhow::Context;
use std::process::Command;

pub fn get(service: &str, property: &str) -> anyhow::Result<String> {
    let output = Command::new("lipc-get-prop")
        .args([service, property])
        .output()
        .context("lipc-get-prop")?;
    anyhow::ensure!(output.status.success(), "lipc-get-prop {property} failed");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn set(service: &str, property: &str, value: &str) -> anyhow::Result<()> {
    let status = Command::new("lipc-set-prop")
        .args([service, property, value])
        .status()
        .context("lipc-set-prop")?;
    anyhow::ensure!(status.success(), "lipc-set-prop {property} failed");
    Ok(())
}
