// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::datetime::parse_datetime;
use crate::error::{self, Result};
use crate::source::parse_key_source;
use chrono::{DateTime, Utc};
use clap::Parser;
use snafu::ResultExt;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use tough::editor::targets::TargetsEditor;
use tough::key_source::KeySource;
use tough::schema::decoded::Decoded;
use tough::schema::decoded::Hex;
use tough::schema::key::Key;

#[derive(Debug, Parser)]
pub(crate) struct CreateRoleArgs {
    /// Key files to sign with
    #[clap(short = 'k', long = "key", required = true, parse(try_from_str = parse_key_source))]
    keys: Vec<Box<dyn KeySource>>,

    /// Expiration of new role file; can be in full RFC 3339 format, or something like 'in
    /// 7 days'
    #[clap(short = 'e', long = "expires", required = true, parse(try_from_str = parse_datetime))]
    expires: DateTime<Utc>,

    /// Version of targets.json file
    #[clap(short = 'v', long = "version")]
    version: NonZeroU64,

    /// The directory where the repository will be written
    #[clap(short = 'o', long = "outdir")]
    outdir: PathBuf,
}

impl CreateRoleArgs {
    pub(crate) async fn run(&self, role: &str) -> Result<()> {
        // create the new role
        let new_role = TargetsEditor::new(role)
            .version(self.version)
            .expires(self.expires)
            .add_key(key_hash_map(&self.keys).await, None)
            .context(error::DelegationStructureSnafu)?
            .sign(&self.keys)
            .await
            .context(error::SignRepoSnafu)?;
        // write the new role
        let metadata_destination_out = &self.outdir.join("metadata");
        new_role
            .write(metadata_destination_out, false)
            .await
            .context(error::WriteRolesSnafu {
                roles: [role.to_string()].to_vec(),
            })?;
        Ok(())
    }
}

async fn key_hash_map(keys: &[Box<dyn KeySource>]) -> HashMap<Decoded<Hex>, Key> {
    let mut key_pairs = HashMap::new();
    for source in keys {
        let key_pair = source.as_sign().await.unwrap().tuf_key();
        key_pairs.insert(key_pair.key_id().unwrap().clone(), key_pair.clone());
    }
    key_pairs
}
