//! RDPSND over `AUDIO_PLAYBACK_DVC`, driven end to end through `DrdynvcServer` and
//! `DrdynvcClient`.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use ironrdp_dvc::{DrdynvcClient, DrdynvcServer, encode_dvc_messages};
use ironrdp_rdpsnd::client::{Rdpsnd, RdpsndClientHandler};
use ironrdp_rdpsnd::dvc::{AUDIO_PLAYBACK_DVC, RdpsndDvcClient, RdpsndDvcServer};
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use ironrdp_rdpsnd::server::{NegotiatedFormat, RdpsndError, RdpsndServer, RdpsndServerHandler};
use ironrdp_svc::{ChannelFlags, SvcMessage, SvcProcessor as _};

fn pcm() -> AudioFormat {
    AudioFormat {
        format: WaveFormat::PCM,
        n_channels: 2,
        n_samples_per_sec: 44100,
        n_avg_bytes_per_sec: 176_400,
        n_block_align: 4,
        bits_per_sample: 16,
        data: None,
    }
}

#[derive(Debug, Default)]
struct Played {
    waves: Vec<(AudioFormat, u32, Vec<u8>)>,
    closed: usize,
}

#[derive(Debug)]
struct Speaker {
    formats: Vec<AudioFormat>,
    played: Arc<Mutex<Played>>,
}

impl RdpsndClientHandler for Speaker {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn wave(&mut self, format: &AudioFormat, ts: u32, data: Cow<'_, [u8]>) {
        self.played
            .lock()
            .unwrap()
            .waves
            .push((format.clone(), ts, data.into_owned()));
    }

    fn set_volume(&mut self, _volume: VolumePdu) {}

    fn set_pitch(&mut self, _pitch: PitchPdu) {}

    fn close(&mut self) {
        self.played.lock().unwrap().closed += 1;
    }
}

#[derive(Debug, Default)]
struct Streamed {
    started: Option<AudioFormat>,
    stopped: bool,
}

#[derive(Debug)]
struct Source {
    formats: Vec<AudioFormat>,
    streamed: Arc<Mutex<Streamed>>,
}

impl RdpsndServerHandler for Source {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn choose_format<'a>(&mut self, common: &'a [NegotiatedFormat]) -> Option<&'a NegotiatedFormat> {
        common.first()
    }

    fn start(&mut self, format: &NegotiatedFormat) -> Result<(), Box<dyn RdpsndError>> {
        self.streamed.lock().unwrap().started = Some(format.format().clone());
        Ok(())
    }

    fn stop(&mut self) {
        self.streamed.lock().unwrap().stopped = true;
    }
}

struct Session {
    server: DrdynvcServer,
    client: DrdynvcClient,
    played: Arc<Mutex<Played>>,
    streamed: Arc<Mutex<Streamed>>,
}

impl Session {
    fn new(client_formats: Vec<AudioFormat>) -> Self {
        let played = Arc::new(Mutex::new(Played::default()));
        let streamed = Arc::new(Mutex::new(Streamed::default()));
        let server =
            DrdynvcServer::new().with_dynamic_channel(RdpsndDvcServer::new(RdpsndServer::new(Box::new(Source {
                formats: vec![pcm()],
                streamed: Arc::clone(&streamed),
            }))));
        let client = DrdynvcClient::new().with_dynamic_channel(RdpsndDvcClient::new(Rdpsnd::new(Box::new(Speaker {
            formats: client_formats,
            played: Arc::clone(&played),
        }))));
        Self {
            server,
            client,
            played,
            streamed,
        }
    }

    /// Deliver server messages to the client and the client's replies back, until both
    /// sides are quiet.
    fn exchange(&mut self, mut to_client: Vec<SvcMessage>) {
        for _ in 0..32 {
            if to_client.is_empty() {
                return;
            }
            let mut to_server = Vec::new();
            for message in to_client.drain(..) {
                let payload = message.encode_unframed_pdu().expect("server message should encode");
                to_server.extend(self.client.process(&payload).expect("client should accept it"));
            }
            for message in to_server {
                let payload = message.encode_unframed_pdu().expect("client message should encode");
                to_client.extend(self.server.process(&payload).expect("server should accept it"));
            }
        }
        panic!("the channels never went quiet");
    }

    fn connect(&mut self) {
        let start = self.server.start().expect("DRDYNVC should start");
        self.exchange(start);
    }

    fn channel_id(&self) -> u32 {
        self.server
            .get_channel_id_by_type::<RdpsndDvcServer>()
            .expect("the audio channel should be open")
    }

    fn send<F>(&mut self, build: F)
    where
        F: FnOnce(&mut RdpsndDvcServer) -> ironrdp_pdu::PduResult<Vec<ironrdp_dvc::DvcMessage>>,
    {
        let id = self.channel_id();
        let messages = build(
            self.server
                .dvc_by_id_mut::<RdpsndDvcServer>(id)
                .expect("the audio channel should be open")
                .processor_mut(),
        )
        .expect("the server should encode it");
        let messages = encode_dvc_messages(id, messages, ChannelFlags::SHOW_PROTOCOL).expect("DVC data should encode");
        self.exchange(messages);
    }
}

#[test]
fn channel_is_audio_playback_dvc() {
    let client = RdpsndDvcClient::new(Rdpsnd::new(Box::new(Speaker {
        formats: Vec::new(),
        played: Arc::default(),
    })));
    assert_eq!(ironrdp_dvc::DvcProcessor::channel_name(&client), AUDIO_PLAYBACK_DVC);
    assert_eq!(AUDIO_PLAYBACK_DVC, "AUDIO_PLAYBACK_DVC");
}

#[test]
fn negotiates_and_plays_over_the_dynamic_channel() {
    let mut session = Session::new(vec![pcm()]);
    session.connect();

    assert_eq!(session.streamed.lock().unwrap().started, Some(pcm()));
    let server = session
        .server
        .dvc_by_id::<RdpsndDvcServer>(session.channel_id())
        .expect("the audio channel should be open");
    assert!(server.processor().inner().is_ready());

    session.send(|server| server.wave(vec![1, 2, 3, 4], 42));
    let played = session.played.lock().unwrap();
    assert_eq!(played.waves, vec![(pcm(), 42, vec![1, 2, 3, 4])]);
}

#[test]
fn waves_larger_than_one_dvc_pdu_are_reassembled() {
    let mut session = Session::new(vec![pcm()]);
    session.connect();

    // Well above the 1600-byte DVC data limit, so it goes out as DataFirst + Data PDUs.
    let wave: Vec<u8> = (0..17_640u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    session.send(|server| server.wave(wave.clone(), 7));

    let played = session.played.lock().unwrap();
    assert_eq!(played.waves.len(), 1);
    assert_eq!(played.waves[0].2, wave);
}

#[test]
fn every_wave_is_confirmed_to_the_server() {
    let mut session = Session::new(vec![pcm()]);
    session.connect();

    for ts in 0..3 {
        session.send(|server| server.wave(vec![0; 16], ts));
    }
    let server = session
        .server
        .dvc_by_id::<RdpsndDvcServer>(session.channel_id())
        .expect("the audio channel should be open");
    assert_eq!(server.processor().inner().stats().waves_sent, 3);
    assert_eq!(session.played.lock().unwrap().waves.len(), 3);
}

#[test]
fn a_client_without_the_format_gets_no_stream() {
    let mut other = pcm();
    other.n_samples_per_sec = 22050;
    other.n_avg_bytes_per_sec = 88200;
    let mut session = Session::new(vec![other]);
    session.connect();

    // The channel opens, but with no common format the handler is never started.
    assert_eq!(session.streamed.lock().unwrap().started, None);
    assert!(session.played.lock().unwrap().waves.is_empty());
}

#[test]
fn close_reaches_the_client() {
    let mut session = Session::new(vec![pcm()]);
    session.connect();

    session.send(RdpsndDvcServer::close_stream);
    assert!(session.played.lock().unwrap().closed >= 1);
}

#[test]
fn a_client_without_the_channel_refuses_it() {
    let streamed = Arc::new(Mutex::new(Streamed::default()));
    let mut server =
        DrdynvcServer::new().with_dynamic_channel(RdpsndDvcServer::new(RdpsndServer::new(Box::new(Source {
            formats: vec![pcm()],
            streamed: Arc::clone(&streamed),
        }))));
    let mut client = DrdynvcClient::new();

    let mut to_client = server.start().expect("DRDYNVC should start");
    for _ in 0..4 {
        let mut to_server = Vec::new();
        for message in to_client.drain(..) {
            let payload = message.encode_unframed_pdu().expect("server message should encode");
            to_server.extend(client.process(&payload).expect("client should answer"));
        }
        for message in to_server {
            let payload = message.encode_unframed_pdu().expect("client message should encode");
            to_client.extend(server.process(&payload).expect("server should accept the refusal"));
        }
    }

    // The server's formats were never sent, and nothing started.
    assert_eq!(streamed.lock().unwrap().started, None);
    assert!(
        server
            .get_channel_id_by_type::<RdpsndDvcServer>()
            .is_none_or(|id| !server.is_channel_opened(id))
    );
}
