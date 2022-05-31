use crate::pipeline::error::Res;
use prost::alloc::vec::Vec as ProstVec;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Debug)]
pub enum HashMapCommand {
    Write(Uuid, ProstVec<u8>, oneshot::Sender<Option<ProstVec<u8>>>),
    Remove(Uuid, oneshot::Sender<Option<ProstVec<u8>>>),
}
pub struct HashMapHandler {
    name: &'static str,
    m: HashMap<Uuid, ProstVec<u8>>,
    receiver: mpsc::Receiver<HashMapCommand>,
}
impl HashMapHandler {
    #[must_use]
    pub fn new(name: &'static str, receiver: mpsc::Receiver<HashMapCommand>) -> HashMapHandler {
        HashMapHandler {
            name,
            m: HashMap::new(),
            receiver,
        }
    }

    pub async fn run(mut self) {
        while let Some(command) = self.receiver.recv().await {
            let res = match command {
                HashMapCommand::Write(key, value, ack) => self.write(key, value, ack).await,
                HashMapCommand::Remove(key, ack) => self.remove(key, ack).await,
            };
            if res.is_err() {
                println!(
                    "{} could not complete operation on HashMap: {}",
                    self.name,
                    res.unwrap_err()
                );
            }
        }
    }
    async fn write(
        &mut self,
        key: Uuid,
        value: ProstVec<u8>,
        ack: oneshot::Sender<Option<ProstVec<u8>>>,
    ) -> Res<()> {
        println!("{} writing data with key {key}", self.name);
        let ousted = self.m.insert(key, value);
        ack.send(ousted)
            .map_err(|_| mpsc::error::SendError::<Vec<u8>>(vec![]).into())
    }
    async fn remove(&mut self, key: Uuid, ack: oneshot::Sender<Option<ProstVec<u8>>>) -> Res<()> {
        println!("{} removing data with key {key}", self.name);
        let removed = self.m.remove(&key);
        ack.send(removed)
            .map_err(|_| mpsc::error::SendError::<Vec<u8>>(vec![]).into())
    }
}

impl Drop for HashMapHandler {
    fn drop(&mut self) {
        println!("{} closing", self.name);
    }
}
