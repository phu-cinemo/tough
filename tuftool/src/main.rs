// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

#![deny(rust_2018_idioms)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::missing_errors_doc,
    // Identifiers like Command::Create are clearer than Self::Create regardless of context
    clippy::use_self,
    // Caused by interacting with tough::schema::*._extra
    clippy::used_underscore_binding,
    clippy::result_large_err,
)]

mod add_key_role;
mod add_role;
mod clone;
mod common;
mod create;
mod create_role;
mod datetime;
mod download;
mod download_root;
mod error;
mod remove_key_role;
mod remove_role;
mod root;
mod source;
mod transfer_metadata;
mod update;
mod update_targets;

use crate::error::Result;
use clap::Parser;
use futures::{StreamExt, TryStreamExt};
use simplelog::{ColorChoice, ConfigBuilder, LevelFilter, TermLogger, TerminalMode};
use snafu::{ErrorCompat, OptionExt, ResultExt};
use std::collections::HashMap;
use std::path::Path;
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;
use tough::schema::Target;
use tough::TargetName;
use walkdir::WalkDir;

static SPEC_VERSION: &str = "1.0.0";

/// This wrapper enables global options and initializes the logger before running any subcommands.
#[derive(Parser)]
struct Program {
    /// Set logging verbosity [trace|debug|info|warn|error]
    #[clap(
        name = "log-level",
        short = 'l',
        long = "log-level",
        default_value = "info"
    )]
    log_level: LevelFilter,
    #[clap(subcommand)]
    cmd: Command,
}

impl Program {
    async fn run(self) -> Result<()> {
        TermLogger::init(
            self.log_level,
            ConfigBuilder::new()
                .add_filter_allow_str("tuftool")
                .add_filter_allow_str("tough")
                .build(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        )
        .context(error::LoggerSnafu)?;
        self.cmd.run().await
    }
}

#[derive(Debug, Parser)]
enum Command {
    /// Create a TUF repository
    Create(create::CreateArgs),
    /// Download a TUF repository's targets
    Download(download::DownloadArgs),
    /// Update a TUF repository's metadata and optionally add targets
    Update(Box<update::UpdateArgs>),
    /// Manipulate a root.json metadata file
    #[clap(subcommand)]
    Root(root::Command),
    /// Delegation Commands
    Delegation(Delegation),
    /// Clone a TUF repository, including metadata and some or all targets
    Clone(clone::CloneArgs),
    /// Transfer a TUF repository's metadata from a previous root to a new root
    TransferMetadata(transfer_metadata::TransferMetadataArgs),
}

impl Command {
    async fn run(self) -> Result<()> {
        match self {
            Command::Create(args) => args.run().await,
            Command::Root(root_subcommand) => root_subcommand.run().await,
            Command::Download(args) => args.run().await,
            Command::Update(args) => args.run().await,
            Command::Delegation(cmd) => cmd.run().await,
            Command::Clone(cmd) => cmd.run().await,
            Command::TransferMetadata(cmd) => cmd.run().await,
        }
    }
}

async fn load_file<T>(path: &Path) -> Result<T>
where
    for<'de> T: serde::Deserialize<'de>,
{
    serde_json::from_slice(
        &tokio::fs::read(path)
            .await
            .context(error::FileOpenSnafu { path })?,
    )
    .context(error::FileParseJsonSnafu { path })
}

async fn write_file<T>(path: &Path, json: &T) -> Result<()>
where
    T: serde::Serialize,
{
    // Use `tempfile::NamedTempFile::persist` to perform an atomic file write.
    let parent = path.parent().context(error::PathParentSnafu { path })?;
    let file =
        NamedTempFile::new_in(parent).context(error::FileTempCreateSnafu { path: parent })?;

    let (file, tmp_path) = file.into_parts();
    let mut file = tokio::fs::File::from_std(file);

    let buf = serde_json::to_vec_pretty(json).context(error::FileWriteJsonSnafu { path })?;
    file.write_all(&buf)
        .await
        .context(error::FileWriteSnafu { path })?;

    let file = file.into_std().await;
    NamedTempFile::from_parts(file, tmp_path)
        .persist(path)
        .context(error::FilePersistSnafu { path })?;

    Ok(())
}

// Walk the directory specified, building a map of filename to Target structs.
// Hashing of the targets is done in parallel
async fn build_targets<P>(indir: P, follow_links: bool) -> Result<HashMap<TargetName, Target>>
where
    P: AsRef<Path>,
{
    let indir = indir.as_ref().to_owned();

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let indir_clone = indir.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let walker = WalkDir::new(indir_clone.clone()).follow_links(follow_links);

        for entry in walker {
            if tx.blocking_send(entry).is_err() {
                // Receiver error'ed out
                break;
            };
        }
        Ok(())
    });

    // Spawn tasks to process targets concurrently.
    futures::stream::unfold(
        rx,
        move |mut rx| async move { Some((rx.recv().await?, rx)) },
    )
    .filter_map(|entry| {
        let indir = indir.clone();
        async move {
            match entry {
                Ok(entry) => {
                    if entry.file_type().is_file() {
                        let future = async move { process_target(entry.path()).await };
                        Some(tokio::task::spawn(future).await.unwrap())
                    } else {
                        None
                    }
                }
                Err(err) => Some(Err(err).context(error::WalkDirSnafu { directory: indir })),
            }
        }
    })
    .try_collect()
    .await
}

async fn process_target(path: &Path) -> Result<(TargetName, Target)> {
    // Get the file name as a TargetName
    let target_name = TargetName::new(
        path.file_name()
            .context(error::NoFileNameSnafu { path })?
            .to_str()
            .context(error::PathUtf8Snafu { path })?,
    )
    .context(error::InvalidTargetNameSnafu)?;

    // Build a Target from the path given. If it is not a file, this will fail
    let target = Target::from_path(path)
        .await
        .context(error::TargetFromPathSnafu { path })?;

    Ok((target_name, target))
}

#[tokio::main]
async fn main() -> ! {
    std::process::exit(match Program::from_args().run().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            if let Some(var) = std::env::var_os("RUST_BACKTRACE") {
                if var != "0" {
                    if let Some(backtrace) = err.backtrace() {
                        eprintln!("\n{backtrace:?}");
                    }
                }
            }
            1
        }
    })
}

#[derive(Parser, Debug)]
struct Delegation {
    /// The signing role
    #[clap(long = "signing-role", required = true)]
    role: String,

    #[clap(subcommand)]
    cmd: DelegationCommand,
}

impl Delegation {
    async fn run(self) -> Result<()> {
        self.cmd.run(&self.role).await
    }
}

#[derive(Debug, Parser)]
enum DelegationCommand {
    /// Creates a delegated role
    CreateRole(Box<create_role::CreateRoleArgs>),
    /// Add delegated role
    AddRole(Box<add_role::AddRoleArgs>),
    /// Update Delegated targets
    UpdateDelegatedTargets(Box<update_targets::UpdateTargetsArgs>),
    /// Add a key to a delegated role
    AddKey(Box<add_key_role::AddKeyArgs>),
    /// Remove a key from a delegated role
    RemoveKey(Box<remove_key_role::RemoveKeyArgs>),
    /// Remove a role
    Remove(Box<remove_role::RemoveRoleArgs>),
}

impl DelegationCommand {
    async fn run(self, role: &str) -> Result<()> {
        match self {
            DelegationCommand::CreateRole(args) => args.run(role).await,
            DelegationCommand::AddRole(args) => args.run(role).await,
            DelegationCommand::UpdateDelegatedTargets(args) => args.run(role).await,
            DelegationCommand::AddKey(args) => args.run(role).await,
            DelegationCommand::RemoveKey(args) => args.run(role).await,
            DelegationCommand::Remove(args) => args.run(role).await,
        }
    }
}
