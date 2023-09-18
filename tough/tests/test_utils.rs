// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0

// An integration test might want to use some, but not all of, the symbols herein. To do so would
// cause compiler warnings for unused code, so we suppress them.
#![allow(unused)]

use futures::TryStreamExt;
use futures_core::Stream;
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

/// Utilities for tests. Not every test module uses every function, so we suppress unused warnings.

pub const DATA_1: &str = "123\n456\n789\n0\n";
pub const DATA_2: &str = "abc\ndef\nhij\nk\n";
pub const DATA_3: &str = "!@#\n$%^\n&*(\n)\n";

/// Returns the path to our test data directory
pub fn test_data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
}

/// Converts a filepath into a URI formatted string
pub fn dir_url<P: AsRef<Path>>(path: P) -> Url {
    Url::from_directory_path(path).unwrap()
}

/// Gets the goods from a read and makes a Vec
pub async fn read_to_end<S, T, E>(mut stream: S) -> Vec<u8>
where
    S: Stream<Item = Result<T, E>> + Send,
    T: AsRef<[u8]>,
    E: std::error::Error,
{
    stream
        .try_fold(Vec::new(), |mut acc, bytes| {
            acc.extend(bytes.as_ref());
            std::future::ready(Ok(acc))
        })
        .await
        .unwrap()
}
