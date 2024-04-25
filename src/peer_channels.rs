//! Peer channels for realtime communication in webxdcs.
use anyhow::{anyhow, Context as _, Result};
use iroh_gossip::net::{Gossip, JoinTopicFut, GOSSIP_ALPN};
use iroh_gossip::proto::{Event as IrohEvent, TopicId};
use iroh_net::magic_endpoint::accept_conn;
use iroh_net::relay::{RelayMap, RelayUrl};
use iroh_net::{key::SecretKey, relay::RelayMode, MagicEndpoint};
use iroh_net::{NodeAddr, NodeId};
use rand::{thread_rng, Rng};
use url::Url;

use crate::chat::send_msg;
use crate::config::Config;
use crate::context::Context;
use crate::message::{Message, MsgId, Viewtype};
use crate::mimeparser::SystemMessage;
use crate::EventType;

impl Context {
    /// Create magic endpoint and gossip for the context.
    pub(crate) async fn inite_peer_channels(&self) -> Result<()> {
        let secret_key: SecretKey = self.get_or_generate_iroh_keypair().await?;
        info!(self, "Iroh secret key: {}", secret_key.to_string());

        let mut ctx_gossip = self.gossip.lock().await;
        if ctx_gossip.is_some() {
            warn!(
                self,
                "Tried to create endpoint even though there already is one"
            );
            return Ok(());
        }

        let endpoint = Box::pin(
            MagicEndpoint::builder()
                .secret_key(secret_key)
                .alpns(vec![GOSSIP_ALPN.to_vec()])
                .relay_mode(
                    /* self.metadata
                    .read()
                    .await
                    .as_ref()
                    .map(|conf| {
                        let url = conf
                            .iroh_relay
                            .as_deref()
                            .unwrap_or("https://iroh.testrun.org:4443");
                        let url = RelayUrl::from(Url::parse(url)?);
                        Ok::<_, url::ParseError>(RelayMode::Custom(RelayMap::from_url(url)))
                    })
                    .transpose()?
                    // This should later be RelayMode::Disable as soon as chatmail servers have relay servers
                    .unwrap_or(RelayMode::Default), */
                    RelayMode::Custom(RelayMap::from_url(RelayUrl::from(
                        Url::parse("https://iroh.testrun.org:4443").unwrap(),
                    ))),
                )
                .bind(0),
        )
        .await?;

        // create gossip
        let my_addr = endpoint.my_addr().await?;
        let gossip = Gossip::from_endpoint(endpoint.clone(), Default::default(), &my_addr.info);

        // spawn endpoint loop that forwards incoming connections to the gossiper
        let context = self.clone();
        tokio::spawn(endpoint_loop(context, endpoint.clone(), gossip.clone()));

        *ctx_gossip = Some(gossip.clone());
        *self.endpoint.lock().await = Some(endpoint);

        Ok(())
    }

    /// Join a topic and create the subscriber loop for it.
    ///
    /// If there is no gossip, create it.
    ///
    /// The returned future resolves when the swarm becomes operational.
    async fn join_and_subscribe_gossip(&self, msg_id: MsgId) -> Result<JoinTopicFut> {
        let mut gossip = (*self.gossip.lock().await).clone();
        if gossip.is_none() {
            self.inite_peer_channels().await?;
            gossip.clone_from(&(*self.gossip.lock().await));
        }

        let gossip = gossip.context("no gossip")?;
        let peers = self.get_gossip_peers(msg_id).await?;

        // connect to all peers
        for peer in &peers {
            self.endpoint
                .lock()
                .await
                .as_ref()
                .context("iroh endpoint not initialized")?
                .add_node_addr(peer.clone())?;
        }

        let topic = self.get_topic_for_msg_id(msg_id).await?;
        let connect_future = gossip
            .join(topic, peers.into_iter().map(|addr| addr.node_id).collect())
            .await?;

        tokio::spawn(subscribe_loop(self.clone(), gossip.clone(), topic, msg_id));

        Ok(connect_future)
    }

    /// Cache a peers [NodeId] for one topic.
    pub(crate) async fn add_peer_for_topic(
        &self,
        msg_id: MsgId,
        topic: TopicId,
        peer: NodeId,
        relay_server: Option<&str>,
    ) -> Result<()> {
        self.sql
            .execute(
                "INSERT OR IGNORE INTO iroh_gossip_peers (msg_id, public_key, topic, relay_server) VALUES (?, ?, ?, ?)",
                (msg_id, peer.as_bytes(), topic.as_bytes(), relay_server),
            )
            .await?;
        Ok(())
    }

    /// Get a list of [NodeAddr]s for one webxdc.
    async fn get_gossip_peers(&self, msg_id: MsgId) -> Result<Vec<NodeAddr>> {
        self.sql
            .query_map(
                "SELECT public_key, relay_server FROM iroh_gossip_peers WHERE msg_id = ? AND public_key != ?",
                (msg_id, self.get_iroh_node_addr().await?.node_id.as_bytes()),
                |row| {
                    let key = row.get::<_, Vec<u8>>(0)?;
                    let server = row.get::<_, Option<String>>(1)?;
                    Ok((key, server))
                },
                |g| {
                    g.map(|data| {
                        let (key, server) = data?;
                        let server = server.map(|data| Ok::<_, url::ParseError>(RelayUrl::from(Url::parse(&data)?))).transpose()?;
                        let id = NodeId::from_bytes(&key.try_into()
                        .map_err(|_| anyhow!("Can't convert sql data to [u8; 32]"))?)?;
                        Ok::<_, anyhow::Error>(NodeAddr::from_parts(
                            id, server, vec![]
                        ))
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(Into::into)
                },
            )
            .await
    }

    /// Remove one cached peer from a topic.
    async fn _delete_webxdc_gossip_peer_for_msg(&self, topic: TopicId, peer: NodeId) -> Result<()> {
        self.sql
            .execute(
                "DELETE FROM iroh_gossip_peers WHERE public_key = ? topic = ?",
                (peer.as_bytes(), topic.as_bytes()),
            )
            .await?;
        Ok(())
    }

    /// Get the iroh secret key from the database or generate a new one and persist it.
    pub(crate) async fn get_or_generate_iroh_keypair(&self) -> Result<SecretKey> {
        match self.get_config_parsed(Config::IrohSecretKey).await? {
            Some(key) => Ok(key),
            None => {
                let key = SecretKey::generate();
                self.set_config_internal(Config::IrohSecretKey, Some(&key.to_string()))
                    .await?;
                Ok(key)
            }
        }
    }

    /// Get the iroh [NodeAddr] without direct IP addresses.
    pub(crate) async fn get_iroh_node_addr(&self) -> Result<NodeAddr> {
        let endpoint = self.endpoint.lock().await;
        let relay = endpoint
            .as_ref()
            .context("iroh endpoint not initialized")?
            .my_relay();
        Ok(NodeAddr::from_parts(
            endpoint
                .as_ref()
                .context("iroh endpoint not initialized")?
                .node_id(),
            relay,
            vec![],
        ))
    }

    /// Get the topic for a given [MsgId].
    pub(crate) async fn get_topic_for_msg_id(&self, msg_id: MsgId) -> Result<TopicId> {
        let bytes = self
            .sql
            .query_row(
                "SELECT topic FROM iroh_gossip_peers WHERE msg_id = ? LIMIT 1",
                (msg_id,),
                |row| {
                    let data = row.get::<_, Vec<u8>>(0)?;
                    Ok(data)
                },
            )
            .await?;
        Ok(TopicId::from_bytes(bytes.try_into().unwrap()))
    }

    /// Send realtime data to the gossip swarm.
    pub async fn send_webxdc_realtime_data(&self, msg_id: MsgId, mut data: Vec<u8>) -> Result<()> {
        let topic = self.get_topic_for_msg_id(msg_id).await?;
        let has_joined = self.channels.lock().await.get(&topic).is_none();
        if has_joined {
            self.send_webxdc_realtime_advertisement(msg_id).await?;
        }

        // Wrapped because rng is not `send`
        // Create some salt so that messages are not deduplicated by iroh.
        let data = {
            let mut rng = thread_rng();
            let mut salt = [0u8; 8];
            rng.fill(&mut salt[..]);
            data.extend(salt);
            data
        };

        self.gossip
            .lock()
            .await
            .as_ref()
            .context("No gossip")?
            .broadcast(topic, data.into())
            .await?;
        Ok(())
    }

    /// Send a gossip advertisement to the chat that [MsgId] belongs to.
    /// Automatically join the gossip for the [MsgId] if not already joined.
    /// Creates magic endpoint and gossip if not already created.
    /// This method should be called from the frontend when `setRealtimeListener` is called.
    pub async fn send_webxdc_realtime_advertisement(
        &self,
        msg_id: MsgId,
    ) -> Result<Option<JoinTopicFut>> {
        let topic = self.get_topic_for_msg_id(msg_id).await?;
        let mut channels = self.channels.lock().await;
        let fut = if channels.get(&topic).is_some() {
            return Ok(None);
        } else {
            channels.insert(topic);
            self.join_and_subscribe_gossip(msg_id).await?
        };
        drop(channels);

        let mut msg = Message::new(Viewtype::Text);
        msg.hidden = true;
        let webxdc = Message::load_from_db(self, msg_id).await?;
        msg.param.set_cmd(SystemMessage::IrohNodeAddr);
        msg.in_reply_to = Some(webxdc.rfc724_mid.clone());
        send_msg(self, webxdc.chat_id, &mut msg).await?;
        Ok(Some(fut))
    }

    /// Leave the gossip of the webxdc with given [MsgId].
    pub async fn leave_gossip(&self, msg_id: MsgId) -> Result<()> {
        let topic = self.get_topic_for_msg_id(msg_id).await?;
        let gossip = self.gossip.lock().await;
        gossip.as_ref().context("No gossip")?.quit(topic).await?;
        self.channels.lock().await.remove(&topic);
        info!(self, "Left gossip for {msg_id}");
        Ok(())
    }
}

pub(crate) fn create_random_topic() -> TopicId {
    TopicId::from_bytes(rand::random())
}

async fn endpoint_loop(context: Context, endpoint: MagicEndpoint, gossip: Gossip) {
    while let Some(conn) = endpoint.accept().await {
        info!(context, "accepting connection with {:?}", conn);
        let gossip = gossip.clone();
        let context = context.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(&context, conn, gossip).await {
                warn!(context, "iroh connection error: {err}");
            }
        });
    }
}

async fn handle_connection(
    context: &Context,
    conn: quinn::Connecting,
    gossip: Gossip,
) -> anyhow::Result<()> {
    let (peer_id, alpn, conn) = accept_conn(conn).await?;
    match alpn.as_bytes() {
        GOSSIP_ALPN => gossip
            .handle_connection(conn)
            .await
            .context(format!("Connection to {peer_id} with ALPN {alpn} failed"))?,
        _ => info!(
            context,
            "Ignoring connection from {peer_id}: unsupported ALPN protocol"
        ),
    }
    Ok(())
}

async fn subscribe_loop(
    context: Context,
    gossip: Gossip,
    topic: TopicId,
    msg_id: MsgId,
) -> Result<()> {
    let mut stream = gossip.subscribe(topic).await?;
    loop {
        let event = stream.recv().await?;
        match event {
            IrohEvent::NeighborUp(node) => {
                info!(context, "NeighborUp: {:?}", node);
                context
                    .add_peer_for_topic(msg_id, topic, node, None)
                    .await?;
            }
            IrohEvent::Received(event) => {
                info!(context, "Received: {:?}", event);
                context.emit_event(EventType::WebxdcRealtimeData {
                    msg_id,
                    data: event.content[0..event.content.len() - 8].into(),
                });
            }
            _ => (),
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        chat::send_msg,
        message::{Message, Viewtype},
        test_utils::TestContextManager,
        EventType,
    };
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_can_communicate() {
        let mut tcm = TestContextManager::new();
        let alice = &mut tcm.alice().await;
        let bob = &mut tcm.bob().await;
        alice.ctx.start_io().await;
        bob.ctx.start_io().await;

        // Alice sends webxdc to bob
        let alice_chat = alice.create_chat(bob).await;
        let mut instance = Message::new(Viewtype::File);
        instance
            .set_file_from_bytes(
                alice,
                "minimal.xdc",
                include_bytes!("../test-data/webxdc/minimal.xdc"),
                None,
            )
            .await
            .unwrap();

        send_msg(alice, alice_chat.id, &mut instance).await.unwrap();
        let alice_webxdc = alice.get_last_msg().await;
        assert_eq!(alice_webxdc.get_viewtype(), Viewtype::Webxdc);

        let webxdc = alice.pop_sent_msg().await;
        let bob_webdxc = bob.recv_msg(&webxdc).await;
        assert_eq!(bob_webdxc.get_viewtype(), Viewtype::Webxdc);

        bob_webdxc.chat_id.accept(bob).await.unwrap();

        // Alice advertises herself.
        alice
            .send_webxdc_realtime_advertisement(alice_webxdc.id)
            .await
            .unwrap();

        bob.recv_msg(&alice.pop_sent_msg().await).await;
        bob.inite_peer_channels().await.unwrap();
        // Bob adds alice to gossip peers.
        let members = bob
            .get_gossip_peers(bob_webdxc.id)
            .await
            .unwrap()
            .into_iter()
            .map(|addr| addr.node_id)
            .collect::<Vec<_>>();
        assert_eq!(
            members,
            vec![alice.get_iroh_node_addr().await.unwrap().node_id]
        );

        bob.join_and_subscribe_gossip(bob_webdxc.id)
            .await
            .unwrap()
            .await
            .unwrap();

        // Alice sends ephemeral message
        alice
            .send_webxdc_realtime_data(alice_webxdc.id, "alice -> bob".as_bytes().to_vec())
            .await
            .unwrap();

        loop {
            let event = bob.evtracker.recv().await.unwrap();
            if let EventType::WebxdcRealtimeData { data, .. } = event.typ {
                if data == "alice -> bob".as_bytes() {
                    break;
                } else {
                    panic!(
                        "Unexpected status update: {}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }

        // Bob sends ephemeral message
        bob.send_webxdc_realtime_data(bob_webdxc.id, "alice -> bob".as_bytes().to_vec())
            .await
            .unwrap();

        loop {
            let event = alice.evtracker.recv().await.unwrap();
            if let EventType::WebxdcRealtimeData { data, .. } = event.typ {
                if data == "alice -> bob".as_bytes() {
                    break;
                } else {
                    panic!(
                        "Unexpected status update: {}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }

        // Alice adds bob to gossip peers.
        let members = alice
            .get_gossip_peers(alice_webxdc.id)
            .await
            .unwrap()
            .into_iter()
            .map(|addr| addr.node_id)
            .collect::<Vec<_>>();

        assert_eq!(
            members,
            vec![bob.get_iroh_node_addr().await.unwrap().node_id]
        );

        bob.send_webxdc_realtime_data(bob_webdxc.id, "bob -> alice 2".as_bytes().to_vec())
            .await
            .unwrap();

        loop {
            let event = alice.evtracker.recv().await.unwrap();
            if let EventType::WebxdcRealtimeData { data, .. } = event.typ {
                if data == "bob -> alice 2".as_bytes() {
                    break;
                } else {
                    panic!(
                        "Unexpected status update: {}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_can_reconnect() {
        let mut tcm = TestContextManager::new();
        let alice = &mut tcm.alice().await;
        let bob = &mut tcm.bob().await;
        alice.ctx.start_io().await;
        bob.ctx.start_io().await;

        // Alice sends webxdc to bob
        let alice_chat = alice.create_chat(bob).await;
        let mut instance = Message::new(Viewtype::File);
        instance
            .set_file_from_bytes(
                alice,
                "minimal.xdc",
                include_bytes!("../test-data/webxdc/minimal.xdc"),
                None,
            )
            .await
            .unwrap();

        send_msg(alice, alice_chat.id, &mut instance).await.unwrap();
        let alice_webxdc = alice.get_last_msg().await;
        assert_eq!(alice_webxdc.get_viewtype(), Viewtype::Webxdc);

        let webxdc = alice.pop_sent_msg().await;
        let bob_webdxc = bob.recv_msg(&webxdc).await;
        assert_eq!(bob_webdxc.get_viewtype(), Viewtype::Webxdc);

        bob_webdxc.chat_id.accept(bob).await.unwrap();

        // Alice advertises herself.
        alice
            .send_webxdc_realtime_advertisement(alice_webxdc.id)
            .await
            .unwrap();

        bob.recv_msg(&alice.pop_sent_msg().await).await;

        let fut = bob.join_and_subscribe_gossip(bob_webdxc.id).await.unwrap();
        alice
            .join_and_subscribe_gossip(alice_webxdc.id)
            .await
            .unwrap()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), fut)
            .await
            .unwrap()
            .unwrap();

        // Alice sends ephemeral message
        alice
            .send_webxdc_realtime_data(alice_webxdc.id, "alice -> bob".as_bytes().to_vec())
            .await
            .unwrap();

        loop {
            let event = bob.evtracker.recv().await.unwrap();
            if let EventType::WebxdcRealtimeData { data, .. } = event.typ {
                if data == "alice -> bob".as_bytes() {
                    break;
                } else {
                    panic!(
                        "Unexpected status update: {}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }

        bob.leave_gossip(bob_webdxc.id).await.unwrap();

        bob.join_and_subscribe_gossip(bob_webdxc.id)
            .await
            .unwrap()
            .await
            .unwrap();

        bob.send_webxdc_realtime_data(bob_webdxc.id, "bob -> alice".as_bytes().to_vec())
            .await
            .unwrap();

        loop {
            let event = alice.evtracker.recv().await.unwrap();
            if let EventType::WebxdcRealtimeData { data, .. } = event.typ {
                if data == "bob -> alice".as_bytes() {
                    break;
                } else {
                    panic!(
                        "Unexpected status update: {}",
                        String::from_utf8_lossy(&data)
                    );
                }
            }
        }

        // channel is only used to remeber if an advertisement has been sent
        // bob for example does not change the channels because he never sends an
        // advertisement
        assert_eq!(alice.channels.lock().await.len(), 1);
        alice.leave_gossip(alice_webxdc.id).await.unwrap();
        assert_eq!(alice.channels.lock().await.len(), 0);
    }
}