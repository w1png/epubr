use std::cell::RefCell;
use std::io::Cursor;

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use super::*;

const ASSET: Asset = Asset {
    name: "fixture.onnx",
    path: "fixture.onnx",
    bytes: 128 * 1024,
    sha256: "b44ffb72fcc259676bd80495fef1b44b808ca8f1ffe1b1706a4d7911b0e31f11",
};
const RELEASE: &str = "http://127.0.0.1/models";

#[derive(Debug)]
struct FixtureConnector(u8);

impl Connector for FixtureConnector {
    type Out = FixtureTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        assert_eq!(details.uri.path(), "/models/fixture.onnx");
        let mut response =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", ASSET.bytes).into_bytes();
        response.resize(response.len() + ASSET.bytes as usize, self.0);
        Ok(Some(FixtureTransport {
            response: Cursor::new(response),
            buffers: LazyBuffers::new(4096, 4096),
        }))
    }
}

#[derive(Debug)]
struct FixtureTransport {
    response: Cursor<Vec<u8>>,
    buffers: LazyBuffers,
}

impl Transport for FixtureTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
        Ok(())
    }

    fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
        let count = self.response.read(self.buffers.input_append_buf())?;
        self.buffers.input_appended(count);
        Ok(count != 0)
    }

    fn is_open(&mut self) -> bool {
        true
    }
}

fn fixture_agent(byte: u8) -> ureq::Agent {
    ureq::Agent::with_parts(
        ureq::Agent::config_builder().proxy(None).build(),
        FixtureConnector(byte),
        DefaultResolver::default(),
    )
}

#[test]
fn streams_and_verifies_before_publishing_with_byte_progress() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(ASSET.path);
    let cancellation = CancellationToken::default();
    let events = RefCell::new(Vec::new());

    download_asset(
        &fixture_agent(b'a'),
        RELEASE,
        &ASSET,
        &path,
        &cancellation,
        &|event| {
            let Progress::ModelDownload {
                asset,
                downloaded,
                total,
            } = event
            else {
                panic!("unexpected model progress event: {event:?}");
            };
            assert_eq!(asset, ASSET.name);
            assert_eq!(total, Some(ASSET.bytes));
            assert_eq!(path.exists(), downloaded == ASSET.bytes);
            events.borrow_mut().push(downloaded);
        },
    )
    .unwrap();

    let events = events.into_inner();
    assert!(events.len() >= 3);
    assert_eq!(events.first(), Some(&0));
    assert_eq!(events.last(), Some(&ASSET.bytes));
    assert!(events.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(is_verified(&path, &ASSET, &cancellation).unwrap());

    fs::write(&path, vec![b'b'; ASSET.bytes as usize]).unwrap();
    assert!(!is_verified(&path, &ASSET, &cancellation).unwrap());
}

#[test]
fn rejects_wrong_checksum_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(ASSET.path);
    let result = download_asset(
        &fixture_agent(b'b'),
        RELEASE,
        &ASSET,
        &path,
        &CancellationToken::default(),
        &|_| {},
    );
    assert!(matches!(result, Err(ModelError::InvalidAsset(asset)) if asset == path));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn cancelling_a_partial_download_removes_staged_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(ASSET.path);
    let cancellation = CancellationToken::default();

    let result = download_asset(
        &fixture_agent(b'a'),
        RELEASE,
        &ASSET,
        &path,
        &cancellation,
        &|event| {
            if matches!(event, Progress::ModelDownload { downloaded, .. } if downloaded > 0) {
                cancellation.cancel();
            }
        },
    );
    assert!(matches!(result, Err(ModelError::Cancelled)));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}
