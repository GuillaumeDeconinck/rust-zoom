/*
 * Copyright 2025 Security Union LLC
 *
 * Licensed under either of
 *
 * * Apache License, Version 2.0
 *   (http://www.apache.org/licenses/LICENSE-2.0)
 * * MIT license
 *   (http://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * Unless you explicitly state otherwise, any contribution intentionally
 * submitted for inclusion in the work by you, as defined in the Apache-2.0
 * license, shall be dual licensed as above, without any additional terms or
 * conditions.
 */

use super::super::connection::{ConnectOptions, Connection};
use super::super::decode::{PeerDecodeManager, PeerStatus};
use crate::crypto::aes::Aes128State;
use crate::crypto::rsa::RsaWrapper;
use crate::diagnostics::{DiagnosticManager, SenderDiagnosticManager};
use anyhow::{anyhow, Result};
use futures::channel::mpsc::UnboundedSender;
use log::{debug, error, info};
use protobuf::Message;
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};
use rsa::RsaPublicKey;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use videocall_types::protos::aes_packet::AesPacket;
use videocall_types::protos::diagnostics_packet::DiagnosticsPacket;
use videocall_types::protos::media_packet::media_packet::MediaType;
use videocall_types::protos::packet_wrapper::packet_wrapper::PacketType;
use videocall_types::protos::packet_wrapper::PacketWrapper;
use videocall_types::protos::rsa_packet::RsaPacket;
use wasm_bindgen::JsValue;
use yew::prelude::Callback;

/// Options struct for constructing a client via [VideoCallClient::new(options)][VideoCallClient::new]
#[derive(Clone, Debug, PartialEq)]
pub struct VideoCallClientOptions {
    /// `true` to use end-to-end encription; `false` to send data unencrypted
    pub enable_e2ee: bool,

    /// `true` to use webtransport, `false` to use websocket
    pub enable_webtransport: bool,

    /// Callback will be called as `callback(peer_userid)` when a new peer is added
    pub on_peer_added: Callback<String>,

    /// Callback will be called as `callback(peer_userid, media_type)` immediately after the first frame of a given peer & media type is decoded
    pub on_peer_first_frame: Callback<(String, MediaType)>,

    /// Callback will be called as `callback(peer_userid)` and must return the DOM id of the
    /// `HtmlCanvasElement` into which the peer video should be rendered
    pub get_peer_video_canvas_id: Callback<String, String>,

    /// Callback will be called as `callback(peer_userid)` and must return the DOM id of the
    /// `HtmlCanvasElement` into which the peer screen image should be rendered
    pub get_peer_screen_canvas_id: Callback<String, String>,

    /// The current client's userid.  This userid will appear as this client's `peer_userid` in the
    /// remote peers' clients.
    pub userid: String,

    /// The url to which WebSocket connections should be made
    pub websocket_url: String,

    /// The url to which WebTransport connections should be made
    pub webtransport_url: String,

    /// Callback will be called as `callback(())` after a new connection is made
    pub on_connected: Callback<()>,

    /// Callback will be called as `callback(())` if a connection gets dropped
    pub on_connection_lost: Callback<JsValue>,

    /// Callback will be called as `callback(stats_string)` with diagnostics information
    pub on_diagnostics_update: Option<Callback<String>>,

    /// Callback will be called as `callback(stats_string)` with sender diagnostics information
    pub on_sender_stats_update: Option<Callback<String>>,

    /// `true` to enable diagnostics collection; `false` to disable
    pub enable_diagnostics: bool,

    /// How often to send diagnostics updates in milliseconds (default: 1000)
    pub diagnostics_update_interval_ms: Option<u64>,

    /// Callback for encoder settings
    pub on_encoder_settings_update: Option<Callback<String>>,
}

#[derive(Debug)]
struct InnerOptions {
    enable_e2ee: bool,
    userid: String,
    on_peer_added: Callback<String>,
}

#[derive(Debug)]
struct Inner {
    options: InnerOptions,
    connection: Option<Connection>,
    aes: Rc<Aes128State>,
    rsa: Rc<RsaWrapper>,
    peer_decode_manager: PeerDecodeManager,
    _diagnostics: Option<Rc<DiagnosticManager>>,
    sender_diagnostics: Option<Rc<SenderDiagnosticManager>>,
}

/// The client struct for a video call connection.
///
/// To use it, first construct the struct using [new(options)][Self::new].  Then when/if desired,
/// create the connection using [connect()][Self::connect].  Once connected, decoding of media from
/// remote peers will start immediately.
///
#[derive(Clone, Debug)]
pub struct VideoCallClient {
    options: VideoCallClientOptions,
    inner: Rc<RefCell<Inner>>,
    aes: Rc<Aes128State>,
    _diagnostics: Option<Rc<DiagnosticManager>>,
}

impl PartialEq for VideoCallClient {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner) && self.options == other.options
    }
}

impl VideoCallClient {
    /// Constructor for the client struct.
    ///
    /// See [VideoCallClientOptions] for description of the options.
    ///
    pub fn new(options: VideoCallClientOptions) -> Self {
        let aes = Rc::new(Aes128State::new(options.enable_e2ee));

        // Create diagnostics manager if enabled
        let diagnostics = if options.enable_diagnostics {
            let diagnostics = Rc::new(DiagnosticManager::new(options.userid.clone()));

            // Set up diagnostics callback if provided
            if let Some(callback) = &options.on_diagnostics_update {
                diagnostics.set_stats_callback(callback.clone());
            }

            // Set update interval if provided
            if let Some(interval) = options.diagnostics_update_interval_ms {
                let mut diag = DiagnosticManager::new(options.userid.clone());
                diag.set_reporting_interval(interval);
                let diagnostics = Rc::new(diag);

                // Set up diagnostics callback if provided
                if let Some(callback) = &options.on_diagnostics_update {
                    diagnostics.set_stats_callback(callback.clone());
                }

                Some(diagnostics)
            } else {
                Some(diagnostics)
            }
        } else {
            None
        };

        // Create sender diagnostics manager if diagnostics are enabled
        let sender_diagnostics = if options.enable_diagnostics {
            let sender_diagnostics = Rc::new(SenderDiagnosticManager::new(options.userid.clone()));

            // Set up sender diagnostics callback if provided
            if let Some(callback) = &options.on_sender_stats_update {
                sender_diagnostics.set_stats_callback(callback.clone());
            }

            // Set update interval if provided
            if let Some(interval) = options.diagnostics_update_interval_ms {
                sender_diagnostics.set_reporting_interval(interval);
            }

            Some(sender_diagnostics)
        } else {
            None
        };

        let client = Self {
            options: options.clone(),
            inner: Rc::new(RefCell::new(Inner {
                options: InnerOptions {
                    enable_e2ee: options.enable_e2ee,
                    userid: options.userid.clone(),
                    on_peer_added: options.on_peer_added.clone(),
                },
                connection: None,
                aes: aes.clone(),
                rsa: Rc::new(RsaWrapper::new(options.enable_e2ee)),
                peer_decode_manager: Self::create_peer_decoder_manager(
                    &options,
                    diagnostics.clone(),
                ),
                _diagnostics: diagnostics.clone(),
                sender_diagnostics: sender_diagnostics.clone(),
            })),
            aes,
            _diagnostics: diagnostics.clone(),
        };

        // Set up the packet forwarding from DiagnosticManager to VideoCallClient
        if let Some(diagnostics) = &diagnostics {
            let client_clone = client.clone();
            diagnostics.set_packet_handler(Callback::from(move |packet| {
                client_clone.send_diagnostic_packet(packet);
            }));
        }

        client
    }

    /// Initiates a connection to a videocall server.
    ///
    /// Initiates a connection using WebTransport (to
    /// [`options.webtransport_url`](VideoCallClientOptions::webtransport_url)) or WebSocket (to
    /// [`options.websocket_url`](VideoCallClientOptions::websocket_url)), based on the value of
    /// [`options.enable_webtransport`](VideoCallClientOptions::enable_webtransport).
    ///
    /// Note that this method's success means only that it succesfully *attempted* initiation of the
    /// connection.  The connection cannot actually be considered to have been succesful until the
    /// [`options.on_connected`](VideoCallClientOptions::on_connected) callback has been invoked.
    ///
    /// If the connection does not succeed, the
    /// [`options.on_connection_lost`](VideoCallClientOptions::on_connection_lost) callback will be
    /// invoked.
    ///
    pub fn connect(&mut self) -> anyhow::Result<()> {
        let options = ConnectOptions {
            userid: self.options.userid.clone(),
            websocket_url: self.options.websocket_url.clone(),
            webtransport_url: self.options.webtransport_url.clone(),
            on_inbound_media: {
                let inner = Rc::downgrade(&self.inner);
                Callback::from(move |packet| {
                    if let Some(inner) = Weak::upgrade(&inner) {
                        match inner.try_borrow_mut() {
                            Ok(mut inner) => inner.on_inbound_media(packet),
                            Err(_) => {
                                error!(
                                    "Unable to borrow inner -- dropping receive packet {:?}",
                                    packet
                                );
                            }
                        }
                    }
                })
            },
            on_connected: {
                let inner = Rc::downgrade(&self.inner);
                let callback = self.options.on_connected.clone();
                Callback::from(move |_| {
                    if let Some(inner) = Weak::upgrade(&inner) {
                        match inner.try_borrow() {
                            Ok(inner) => inner.send_public_key(),
                            Err(_) => {
                                error!("Unable to borrow inner -- not sending public key");
                            }
                        }
                    }
                    callback.emit(());
                })
            },
            on_connection_lost: self.options.on_connection_lost.clone(),
            peer_monitor: {
                let inner = Rc::downgrade(&self.inner);
                let on_connection_lost = self.options.on_connection_lost.clone();
                Callback::from(move |_| {
                    if let Some(inner) = Weak::upgrade(&inner) {
                        match inner.try_borrow_mut() {
                            Ok(mut inner) => {
                                inner.peer_decode_manager.run_peer_monitor();
                            }
                            Err(_) => {
                                on_connection_lost.emit(JsValue::from_str(
                                    "Unable to borrow inner -- not starting peer monitor",
                                ));
                            }
                        }
                    }
                })
            },
        };
        info!(
            "webtransport connect = {}",
            self.options.enable_webtransport
        );
        info!(
            "end to end encryption enabled = {}",
            self.options.enable_e2ee
        );

        let mut borrowed = self.inner.try_borrow_mut()?;
        borrowed.connection.replace(Connection::connect(
            self.options.enable_webtransport,
            options,
            self.aes.clone(),
        )?);
        info!("Connected to server");
        Ok(())
    }

    fn create_peer_decoder_manager(
        opts: &VideoCallClientOptions,
        diagnostics: Option<Rc<DiagnosticManager>>,
    ) -> PeerDecodeManager {
        match diagnostics {
            Some(diagnostics) => {
                let mut peer_decode_manager = PeerDecodeManager::new_with_diagnostics(diagnostics);
                peer_decode_manager.on_first_frame = opts.on_peer_first_frame.clone();
                peer_decode_manager.get_video_canvas_id = opts.get_peer_video_canvas_id.clone();
                peer_decode_manager.get_screen_canvas_id = opts.get_peer_screen_canvas_id.clone();
                peer_decode_manager
            }
            None => {
                let mut peer_decode_manager = PeerDecodeManager::new();
                peer_decode_manager.on_first_frame = opts.on_peer_first_frame.clone();
                peer_decode_manager.get_video_canvas_id = opts.get_peer_video_canvas_id.clone();
                peer_decode_manager.get_screen_canvas_id = opts.get_peer_screen_canvas_id.clone();
                peer_decode_manager
            }
        }
    }

    pub(crate) fn send_packet(&self, media: PacketWrapper) {
        match self.inner.try_borrow() {
            Ok(inner) => inner.send_packet(media),
            Err(_) => {
                error!("Unable to borrow inner -- dropping send packet {:?}", media)
            }
        }
    }

    /// Returns `true` if the client is currently connected to a server.
    pub fn is_connected(&self) -> bool {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(connection) = &inner.connection {
                return connection.is_connected();
            }
        };
        false
    }

    /// Returns a vector of the userids of the currently connected remote peers, sorted alphabetically.
    pub fn sorted_peer_keys(&self) -> Vec<String> {
        match self.inner.try_borrow() {
            Ok(inner) => inner.peer_decode_manager.sorted_keys().to_vec(),
            Err(_) => Vec::<String>::new(),
        }
    }

    /// Hacky function that returns true if the given peer has yet to send a frame of screen share.
    ///
    /// No reason for this function to exist, it should be deducible from the
    /// [`options.on_peer_first_frame(key, MediaType::Screen)`](VideoCallClientOptions::on_peer_first_frame)
    /// callback.   Or if polling is really necessary, instead of being hardwired for screen, it'd
    /// be more elegant to at least pass a `MediaType`.
    ///
    pub fn is_awaiting_peer_screen_frame(&self, key: &String) -> bool {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(peer) = inner.peer_decode_manager.get(key) {
                return peer.screen.is_waiting_for_keyframe();
            }
        }
        false
    }

    pub fn is_video_enabled_for_peer(&self, key: &String) -> bool {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(peer) = inner.peer_decode_manager.get(key) {
                return peer.video_enabled;
            }
        }
        false
    }

    pub fn is_screen_share_enabled_for_peer(&self, key: &String) -> bool {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(peer) = inner.peer_decode_manager.get(key) {
                return peer.screen_enabled;
            }
        }
        false
    }

    pub fn is_audio_enabled_for_peer(&self, key: &String) -> bool {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(peer) = inner.peer_decode_manager.get(key) {
                return peer.audio_enabled;
            }
        }
        false
    }

    pub(crate) fn aes(&self) -> Rc<Aes128State> {
        self.aes.clone()
    }

    /// Returns a reference to a copy of [`options.userid`](VideoCallClientOptions::userid)
    pub fn userid(&self) -> &String {
        &self.options.userid
    }

    /// Get diagnostics information for all peers
    ///
    /// Returns a formatted string with FPS stats for all peers if diagnostics are enabled,
    /// or None if diagnostics are disabled.
    pub fn get_diagnostics(&self) -> Option<String> {
        self.inner.borrow().peer_decode_manager.get_all_fps_stats()
    }

    /// Get the FPS for a specific peer and media type
    ///
    /// Returns the current frames per second for the specified peer and media type,
    /// or 0.0 if diagnostics are disabled or the peer doesn't exist.
    pub fn get_peer_fps(&self, peer_id: &str, media_type: MediaType) -> f64 {
        self.inner
            .borrow()
            .peer_decode_manager
            .get_fps(peer_id, media_type)
    }

    /// Send a diagnostic packet to the server
    pub fn send_diagnostic_packet(&self, packet: DiagnosticsPacket) {
        if let Ok(inner) = self.inner.try_borrow() {
            inner.send_packet(PacketWrapper {
                packet_type: PacketType::DIAGNOSTICS.into(),
                email: self.options.userid.clone(),
                data: packet.write_to_bytes().unwrap(),
                ..Default::default()
            });
        } else {
            error!("Failed to borrow inner for sending diagnostic packet");
        }
    }

    pub fn subscribe_diagnostics(
        &self,
        tx: UnboundedSender<DiagnosticsPacket>,
        media_type: MediaType,
    ) {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(sender_diagnostics) = &inner.sender_diagnostics {
                sender_diagnostics.add_sender_channel(tx, media_type);
            }
        }
    }

    pub fn set_video_enabled(&self, enabled: bool) {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(connection) = &inner.connection {
                connection.set_video_enabled(enabled);
            }
        }
    }

    pub fn set_audio_enabled(&self, enabled: bool) {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(connection) = &inner.connection {
                connection.set_audio_enabled(enabled);
            }
        }
    }

    pub fn set_screen_enabled(&self, enabled: bool) {
        if let Ok(inner) = self.inner.try_borrow() {
            if let Some(connection) = &inner.connection {
                connection.set_screen_enabled(enabled);
            }
        }
    }

    /// Updates the speaker device for all connected peers
    ///
    /// This will recreate all audio decoders to use the specified speaker device.
    /// Pass None to use the default system speaker.
    pub fn update_speaker_device(&self, speaker_device_id: Option<String>) -> Result<(), JsValue> {
        match self.inner.try_borrow_mut() {
            Ok(mut inner) => inner
                .peer_decode_manager
                .update_speaker_device(speaker_device_id),
            Err(_) => {
                error!("Failed to borrow inner for updating speaker device");
                Err(JsValue::from_str(
                    "Failed to borrow inner for updating speaker device",
                ))
            }
        }
    }
}

impl Inner {
    fn send_packet(&self, media: PacketWrapper) {
        if let Some(connection) = &self.connection {
            connection.send_packet(media);
        }
    }

    fn on_inbound_media(&mut self, response: PacketWrapper) {
        debug!(
            "<< Received {:?} from {}",
            response.packet_type.enum_value(),
            response.email
        );
        let peer_status = self.peer_decode_manager.ensure_peer(&response.email);
        match response.packet_type.enum_value() {
            Ok(PacketType::AES_KEY) => {
                if !self.options.enable_e2ee {
                    return;
                }
                if let Ok(bytes) = self.rsa.decrypt(&response.data) {
                    debug!("Decrypted AES_KEY from {}", response.email);
                    match AesPacket::parse_from_bytes(&bytes) {
                        Ok(aes_packet) => {
                            if let Err(e) = self.peer_decode_manager.set_peer_aes(
                                &response.email,
                                Aes128State::from_vecs(
                                    aes_packet.key,
                                    aes_packet.iv,
                                    self.options.enable_e2ee,
                                ),
                            ) {
                                error!("Failed to set peer aes: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Failed to parse aes packet: {}", e);
                        }
                    }
                }
            }
            Ok(PacketType::RSA_PUB_KEY) => {
                if !self.options.enable_e2ee {
                    return;
                }
                let encrypted_aes_packet = parse_rsa_packet(&response.data)
                    .and_then(parse_public_key)
                    .and_then(|pub_key| {
                        self.serialize_aes_packet()
                            .map(|aes_packet| (aes_packet, pub_key))
                    })
                    .and_then(|(aes_packet, pub_key)| {
                        self.encrypt_aes_packet(&aes_packet, &pub_key)
                    });

                match encrypted_aes_packet {
                    Ok(data) => {
                        debug!(">> {} sending AES key", self.options.userid);
                        self.send_packet(PacketWrapper {
                            packet_type: PacketType::AES_KEY.into(),
                            email: self.options.userid.clone(),
                            data,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        error!("Failed to send AES_KEY to peer: {}", e);
                    }
                }
            }
            Ok(PacketType::MEDIA) => {
                let email = response.email.clone();
                if let Err(e) = self.peer_decode_manager.decode(response) {
                    error!("error decoding packet: {}", e);
                    self.peer_decode_manager.delete_peer(&email);
                }
            }
            Ok(PacketType::CONNECTION) => {
                error!("Not implemented: CONNECTION packet type");
            }
            Ok(PacketType::DIAGNOSTICS) => {
                // Parse and handle the diagnostics packet
                if let Ok(diagnostics_packet) = DiagnosticsPacket::parse_from_bytes(&response.data)
                {
                    debug!("Received diagnostics packet: {:?}", diagnostics_packet);
                    if let Some(sender_diagnostics) = &self.sender_diagnostics {
                        sender_diagnostics.handle_diagnostic_packet(diagnostics_packet);
                    }
                } else {
                    error!("Failed to parse diagnostics packet");
                }
            }
            Err(e) => {
                error!("Failed to parse diagnostics packet: {}", e);
            }
        }
        if let PeerStatus::Added(peer_userid) = peer_status {
            debug!("added peer {}", peer_userid);
            self.send_public_key();
            self.options.on_peer_added.emit(peer_userid);
        }
    }

    fn send_public_key(&self) {
        if !self.options.enable_e2ee {
            return;
        }
        let userid = self.options.userid.clone();
        let rsa = &*self.rsa;
        match rsa.pub_key.to_public_key_der() {
            Ok(public_key_der) => {
                let packet = RsaPacket {
                    username: userid.clone(),
                    public_key_der: public_key_der.to_vec(),
                    ..Default::default()
                };
                match packet.write_to_bytes() {
                    Ok(data) => {
                        debug!(">> {} sending public key", userid);
                        self.send_packet(PacketWrapper {
                            packet_type: PacketType::RSA_PUB_KEY.into(),
                            email: userid,
                            data,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        error!("Failed to serialize rsa packet: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to export rsa public key to der: {}", e);
            }
        }
    }

    fn serialize_aes_packet(&self) -> Result<Vec<u8>> {
        AesPacket {
            key: self.aes.key.to_vec(),
            iv: self.aes.iv.to_vec(),
            ..Default::default()
        }
        .write_to_bytes()
        .map_err(|e| anyhow!("Failed to serialize aes packet: {}", e.to_string()))
    }

    fn encrypt_aes_packet(&self, aes_packet: &[u8], pub_key: &RsaPublicKey) -> Result<Vec<u8>> {
        self.rsa
            .encrypt_with_key(aes_packet, pub_key)
            .map_err(|e| anyhow!("Failed to encrypt aes packet: {}", e.to_string()))
    }
}

fn parse_rsa_packet(response_data: &[u8]) -> Result<RsaPacket> {
    RsaPacket::parse_from_bytes(response_data)
        .map_err(|e| anyhow!("Failed to parse rsa packet: {}", e.to_string()))
}

fn parse_public_key(rsa_packet: RsaPacket) -> Result<RsaPublicKey> {
    RsaPublicKey::from_public_key_der(&rsa_packet.public_key_der)
        .map_err(|e| anyhow!("Failed to parse rsa public key: {}", e.to_string()))
}
