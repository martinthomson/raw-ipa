use async_trait::async_trait;
use prost::Message;
use raw_ipa::build_async_pipeline;
use raw_ipa::error::{Error, Res};
use raw_ipa::pipeline::async_pipe::{APipeline, AStep, ChannelHelper, SendStr, THelper};
use raw_ipa::proto::pipe::ForwardRequest;
use std::io::Cursor;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio::task::JoinHandle;
use tokio::try_join;
use uuid::Uuid;

/// unchanged from regular pipeline
struct Start {
    uuid: Uuid,
    x: i32,
    y: i32,
}
#[async_trait(?Send)]
impl AStep for Start {
    type Input = ();
    type Output = (i32, i32);

    async fn compute(&self, _: Self::Input, _: &(impl THelper + 'static)) -> Res<Self::Output> {
        Ok((self.x, self.y))
    }

    fn unique_id(&self) -> &Uuid {
        &self.uuid
    }
}

/// unchanged from regular pipeline
struct Add {
    uuid: Uuid,
}
#[async_trait(?Send)]
impl AStep for Add {
    type Input = (i32, i32);
    type Output = i32;

    async fn compute(&self, inp: Self::Input, _: &(impl THelper + 'static)) -> Res<Self::Output> {
        Ok(inp.0 + inp.1)
    }

    fn unique_id(&self) -> &Uuid {
        &self.uuid
    }
}

/// arbitrary async work done (literally a `time::sleep`) to prove that it can occur
struct PairWith3 {
    uuid: Uuid,
}
#[async_trait(?Send)]
impl AStep for PairWith3 {
    type Input = i32;
    type Output = (i32, i32);

    async fn compute(&self, inp: Self::Input, _: &(impl THelper + 'static)) -> Res<Self::Output> {
        let res = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            3
        });
        res.await
            .map_or(Err(Error::Internal), |three| Ok((inp, three)))
    }

    fn unique_id(&self) -> &Uuid {
        &self.uuid
    }
}

struct Stringify {
    uuid: Uuid,
}
#[async_trait(?Send)]
impl AStep for Stringify {
    type Input = i32;
    type Output = String;

    async fn compute(&self, inp: Self::Input, _: &(impl THelper + 'static)) -> Res<Self::Output> {
        Ok(inp.to_string())
    }

    fn unique_id(&self) -> &Uuid {
        &self.uuid
    }
}
struct ForwardData {
    uuid: Uuid,
    receive_uuid: Uuid,
}
#[async_trait(?Send)]
impl AStep for ForwardData {
    type Input = String;
    type Output = String;

    async fn compute(
        &self,
        inp: Self::Input,
        helper: &(impl THelper + 'static),
    ) -> Res<Self::Output> {
        let sent = helper.send_to_next(self.unique_id().to_string(), SendStr(inp.clone()));
        let received = helper.receive_from::<SendStr>(self.receive_uuid.to_string());
        let completed = try_join!(sent, received);
        completed.map(|(_, res)| res.to_string())
    }

    fn unique_id(&self) -> &Uuid {
        &self.uuid
    }
}

struct ExampleAPipeline<H: THelper> {
    helper: H,
}
#[async_trait(?Send)]
impl<H: THelper + 'static> APipeline<(), i32, H> for ExampleAPipeline<H> {
    async fn pipeline(&self, _: ()) -> Res<i32> {
        let pipe = build_async_pipeline!(&self.helper,
            Start { x: 1, y: 2, uuid: Uuid::new_v4() } =>
            Add { uuid: Uuid::new_v4() } =>
            PairWith3 { uuid: Uuid::new_v4() } =>
            Add { uuid: Uuid::new_v4() }
        );
        pipe(()).await
    }
}

struct ForwardingPipeline<H: THelper> {
    helper: H,
    send_uuid: Uuid,
    receive_uuid: Uuid,
}
#[async_trait(?Send)]
impl<H: THelper + 'static> APipeline<(), String, H> for ForwardingPipeline<H> {
    async fn pipeline(&self, _: ()) -> Res<String> {
        let pipe = build_async_pipeline!(&self.helper,
            Start { x: 1, y: 2, uuid: Uuid::new_v4() } =>
            Add { uuid: Uuid::new_v4() } =>
            Stringify { uuid: Uuid::new_v4() } =>
            ForwardData { uuid: self.send_uuid, receive_uuid: self.receive_uuid }
        );
        pipe(()).await
    }
}

#[tokio::main]
async fn main() -> Res<()> {
    let (h1_send, h1_recv) = channel(32);
    let (h2_send, mut h2_recv) = channel(32);
    let (h3_send, _) = channel(32);
    let h1_recv_uuid = Uuid::new_v4();
    let h2_recv_uuid = Uuid::new_v4();
    let run_pipe = tokio::spawn(async move {
        let h1_helper = ChannelHelper::new(h2_send, h3_send, h1_recv);
        let pipe = ForwardingPipeline {
            helper: h1_helper,
            send_uuid: h1_recv_uuid,
            receive_uuid: h2_recv_uuid,
        };
        pipe.pipeline(()).await
    });

    let run_h2_mock: JoinHandle<Res<String>> = tokio::spawn(async move {
        let message = "mocked_h2_data".as_bytes().to_vec();
        let mocked_data = ForwardRequest {
            id: h2_recv_uuid.to_string(),
            num: message,
        };
        let mut buf = Vec::new();
        buf.reserve(mocked_data.encoded_len());
        mocked_data.encode(&mut buf).unwrap();
        h1_send.send(buf).await.map_err(Error::from)?;
        let received_data = h2_recv.recv().await.unwrap();
        let req = ForwardRequest::decode(&mut Cursor::new(received_data.as_slice()))
            .map_err(Error::from)?;
        let str: SendStr = req.num.try_into()?;
        Ok(str.0)
    });
    let (pipe_res, h2_mock_res) = try_join!(run_pipe, run_h2_mock).map_err(Error::from)?;
    println!(
        "pipe output: {}; h2 mocked output: {}",
        pipe_res.unwrap(),
        h2_mock_res.unwrap()
    );
    Ok(())
}
