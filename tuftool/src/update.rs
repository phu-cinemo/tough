// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::build_targets;
use crate::common::UNUSED_URL;
use crate::datetime::parse_datetime;
use crate::error::{self, Result};
use crate::source::parse_key_source;
use chrono::{DateTime, Utc};
use clap::Parser;
use snafu::{OptionExt, ResultExt};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use tough::editor::signed::PathExists;
use tough::editor::RepositoryEditor;
use tough::key_source::KeySource;
use tough::{ExpirationEnforcement, RepositoryLoader};
use url::Url;

#[derive(Debug, Parser)]
pub(crate) struct UpdateArgs {
    /// Key files to sign with
    #[clap(short = 'k', long = "key", required = true, parse(try_from_str = parse_key_source))]
    keys: Vec<Box<dyn KeySource>>,

    /// Version of snapshot.json file
    #[clap(long = "snapshot-version")]
    snapshot_version: NonZeroU64,
    /// Expiration of snapshot.json file; can be in full RFC 3339 format, or something like 'in
    /// 7 days'
    #[clap(long = "snapshot-expires", parse(try_from_str = parse_datetime))]
    snapshot_expires: DateTime<Utc>,

    /// Version of targets.json file
    #[clap(long = "targets-version")]
    targets_version: NonZeroU64,
    /// Expiration of targets.json file; can be in full RFC 3339 format, or something like 'in
    /// 7 days'
    #[clap(long = "targets-expires", parse(try_from_str = parse_datetime))]
    targets_expires: DateTime<Utc>,

    /// Version of timestamp.json file
    #[clap(long = "timestamp-version")]
    timestamp_version: NonZeroU64,
    /// Expiration of timestamp.json file; can be in full RFC 3339 format, or something like 'in
    /// 7 days'
    #[clap(long = "timestamp-expires", parse(try_from_str = parse_datetime))]
    timestamp_expires: DateTime<Utc>,

    /// Path to root.json file for the repository
    #[clap(short = 'r', long = "root")]
    root: PathBuf,

    /// TUF repository metadata base URL
    #[clap(short = 'm', long = "metadata-url")]
    metadata_base_url: Url,

    /// Directory of targets
    #[clap(short = 't', long = "add-targets")]
    targets_indir: Option<PathBuf>,

    /// Behavior when a target exists with the same name and hash in the desired repository
    /// directory, for example from another repository when you're sharing target directories.
    /// Options are "replace", "fail", and "skip"
    #[clap(long = "target-path-exists", default_value = "skip")]
    target_path_exists: PathExists,

    /// Follow symbolic links in the given directory when adding targets
    #[clap(short = 'f', long = "follow")]
    follow: bool,

    /// Number of target hashing threads to run when adding targets
    /// (default: number of cores)
    // No default is specified in structopt here. This is because rayon
    // automatically spawns the same number of threads as cores when any
    // of its parallel methods are called.
    #[clap(short = 'j', long = "jobs")]
    jobs: Option<NonZeroUsize>,

    /// The directory where the updated repository will be written
    #[clap(short = 'o', long = "outdir")]
    outdir: PathBuf,

    /// Incoming metadata from delegatee
    #[clap(short = 'i', long = "incoming-metadata")]
    indir: Option<Url>,

    /// Role of incoming metadata
    #[clap(long = "role")]
    role: Option<String>,

    /// Allow repo download for expired metadata
    #[clap(long)]
    allow_expired_repo: bool,
}

fn expired_repo_warning<P: AsRef<Path>>(path: P) {
    #[rustfmt::skip]
    eprintln!("\
=================================================================
Updating repo at {}
WARNING: `--allow-expired-repo` was passed; this is unsafe and will not establish trust, use only for testing!
=================================================================",
              path.as_ref().display());
}

impl UpdateArgs {
    pub(crate) async fn run(&self) -> Result<()> {
        let expiration_enforcement = if self.allow_expired_repo {
            expired_repo_warning(&self.outdir);
            ExpirationEnforcement::Unsafe
        } else {
            ExpirationEnforcement::Safe
        };
        let repository = RepositoryLoader::new(
            &tokio::fs::read(&self.root)
                .await
                .context(error::OpenRootSnafu { path: &self.root })?,
            self.metadata_base_url.clone(),
            Url::parse(UNUSED_URL).context(error::UrlParseSnafu { url: UNUSED_URL })?,
        )
        .expiration_enforcement(expiration_enforcement)
        .load()
        .await
        .context(error::RepoLoadSnafu)?;
        self.update_metadata(
            RepositoryEditor::from_repo(&self.root, repository)
                .await
                .context(error::EditorFromRepoSnafu { path: &self.root })?,
        )
        .await
    }

    async fn update_metadata(&self, mut editor: RepositoryEditor) -> Result<()> {
        editor
            .targets_version(self.targets_version)
            .context(error::DelegationStructureSnafu)?
            .targets_expires(self.targets_expires)
            .context(error::DelegationStructureSnafu)?
            .snapshot_version(self.snapshot_version)
            .snapshot_expires(self.snapshot_expires)
            .timestamp_version(self.timestamp_version)
            .timestamp_expires(self.timestamp_expires);

        // If the "add-targets" argument was passed, build a list of targets
        // and add them to the repository. If a user specifies job count we
        // override the default, which is the number of cores.
        if let Some(ref targets_indir) = self.targets_indir {
            if let Some(jobs) = self.jobs {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(usize::from(jobs))
                    .build_global()
                    .context(error::InitializeThreadPoolSnafu)?;
            }

            let new_targets = build_targets(targets_indir, self.follow).await?;

            for (target_name, target) in new_targets {
                editor
                    .add_target(target_name, target)
                    .context(error::DelegationStructureSnafu)?;
            }
        };

        // If a `Targets` metadata needs to be updated
        if self.role.is_some() && self.indir.is_some() {
            editor
                .sign_targets_editor(&self.keys)
                .await
                .context(error::DelegationStructureSnafu)?
                .update_delegated_targets(
                    self.role.as_ref().context(error::MissingSnafu {
                        what: "delegated role",
                    })?,
                    self.indir
                        .as_ref()
                        .context(error::MissingSnafu {
                            what: "delegated role metadata url",
                        })?
                        .as_str(),
                )
                .await
                .context(error::DelegateeNotFoundSnafu {
                    role: self.role.as_ref().unwrap().clone(),
                })?;
        }

        // Sign the repo
        let signed_repo = editor
            .sign(&self.keys)
            .await
            .context(error::SignRepoSnafu)?;

        // Symlink any targets that were added
        if let Some(ref targets_indir) = self.targets_indir {
            let targets_outdir = &self.outdir.join("targets");
            signed_repo
                .link_targets(targets_indir, targets_outdir, self.target_path_exists)
                .await
                .context(error::LinkTargetsSnafu {
                    indir: &targets_indir,
                    outdir: targets_outdir,
                })?;
        };

        // Write the metadata to the outdir
        let metadata_dir = &self.outdir.join("metadata");
        signed_repo
            .write(metadata_dir)
            .await
            .context(error::WriteRepoSnafu {
                directory: metadata_dir,
            })?;

        Ok(())
    }
}
