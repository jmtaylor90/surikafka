use super::errors::Error;
use super::futures::{
    Async,
    Future,
    Poll,
    Stream
};
use super::key::KeyGenerator;
use super::rdkafka::{
    ClientContext,
    producer::{
        DeliveryFuture,
        FutureProducer,
        FutureRecord
    }
};

pub struct Writer<C, K, S>
    where C: ClientContext + 'static,
    K: KeyGenerator,
    S: Stream<Item=String, Error=Error>
{
    inner: S,
    topic: String,
    generator: K,
    producer: FutureProducer<C>,
    outstanding: Option<DeliveryFuture>
}

impl<C, K, S> Writer<C, K, S>
    where C: ClientContext + 'static,
          K: KeyGenerator,
          K::Item: Sized,
          S: Stream<Item=String, Error=Error>
{
    pub fn new(
        stream: S,
        topic: String,
        generator: K,
        producer: FutureProducer<C>
    ) -> Writer<C, K, S> {
        Writer {
            inner: stream,
            topic: topic,
            generator: generator,
            producer: producer,
            outstanding: None
        }
    }

    pub fn send(&mut self, msg: &String) -> DeliveryFuture {
        let key = self.generator.generate(msg);
        let record: FutureRecord<K::Item, String> = FutureRecord::to(self.topic.as_ref());
        record.key(&key);
        record.payload(&msg);
        self.producer.send(record, 1000)
    }
}

pub trait WithProduce<S> where S: Stream<Item=String, Error=Error> {
    fn produce<C, K>(
        self,
        topic: String,
        generator: K,
        producer: FutureProducer<C>
    ) -> Writer<C, K, S>
        where C: ClientContext + 'static,
              K: KeyGenerator,
              K::Item: Sized;
}

impl<C, K, S> Stream for Writer<C, K, S>
    where C: ClientContext + 'static,
          K: KeyGenerator,
          K::Item: Sized,
          S: Stream<Item=String, Error=Error>
{
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Error> {
        let outstanding_ready: Poll<Option<(i32, i64)>, Error> = if let Some(f) = self.outstanding.take() {
            let produce_attempt = try_ready!(f.poll());
            match produce_attempt {
                Err( (e, msg) ) => {
                    error!("Failed to produce: {:?}", e);
                    Ok(Async::NotReady)
                }
                Ok( (p, o) ) => {
                    Ok(Async::Ready(Some( (p, o) )))
                }
            }
        } else {
            Ok(Async::Ready(None))
        };

        if let Some( (p, o) ) = try_ready!(outstanding_ready) {
            debug!("Produced to partition {}, offset {}", p, o);
            Ok(Async::Ready(Some(())))
        } else {
            if let Some(msg) = try_ready!(self.inner.poll()) {
                self.outstanding = Some(self.send(&msg));
                Ok(Async::NotReady)
            } else {
                Ok(Async::Ready(None))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use self::super::super::{
        key::StringKeyGenerator,
        futures::{
            Sink,
            sync::mpsc as mpsc
        },
        rdkafka,
        rdkafka::{
            ClientConfig
        },
        tokio
    };
    use std;

    impl WithProduce<mpsc::Receiver<String>> for mpsc::Receiver<String> {
        fn produce<C, K>(
            self,
            topic: String,
            generator: K,
            producer: rdkafka::producer::FutureProducer<C>
        ) -> Writer<C, K, S>
            where C: rdkafka::ClientContext + 'static,
                  K: KeyGenerator,
                  K::Item: Sized
        {
            Writer::new(self, topic, generator, producer)
        }
    }

    #[test]
    fn produces_messages() {
        let mut rt = tokio::runtime::Runtime::new().expect("Failed to build runtime");

        let (mut sender, receiver) = mpsc::channel::<String>(100);

        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", "localhost:9092")
            .set("produce.offset.report", "true")
            .set("message.timeout.ms", "5000")
            .create()
            .expect("Producer creation error");

        let fut_result = receiver
            .produce("test_topic".to_string(), StringKeyGenerator, producer)
            .collect();

        let send_finished = std::thread::spawn(move || {
            sender.send("string1".to_string());
            sender.send("string2".to_string());
            sender.send("string3".to_string());
            sender.close()
        });

        let sent = rt.block_on(fut_result).expect("Failed to send");

        assert_eq!(sent.len(), 3);
    }
}