use async_trait::async_trait;
use std::fmt::Debug;
use std::io::Error;
use std::net::SocketAddr;
use tokio_modbus::client::{Client, Context};
use tokio_modbus::slave::{Slave, SlaveContext};
use tokio_modbus::{Request, Response};

#[async_trait]
pub trait Connector: Send + Debug {
    type Output: Client;

    async fn connect(&self, slave: Slave) -> Result<Self::Output, Error>;
}

pub struct SyncConnector<T: Client, F: Fn(Slave) -> Result<T, Error> + Send + Sync> {
    factory: F,
}

impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send + Sync> SyncConnector<T, F> {
    pub fn new(factory: F) -> Self {
        Self { factory }
    }
}

impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send + Sync> Debug for SyncConnector<T, F> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "SyncConnector()")
    }
}

#[async_trait]
impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send + Sync> Connector for SyncConnector<T, F> {
    type Output = T;

    async fn connect(&self, slave: Slave) -> Result<T, Error> {
        (self.factory)(slave)
    }
}

#[derive(Debug)]
pub struct TcpSlaveConnector {
    socket_addr: SocketAddr,
}

impl TcpSlaveConnector {
    pub fn new(socket_addr: SocketAddr) -> Self {
        Self { socket_addr }
    }
}

#[async_trait]
impl Connector for TcpSlaveConnector {
    type Output = Context;

    async fn connect(&self, slave: Slave) -> Result<Context, Error> {
        tokio_modbus::client::tcp::connect_slave(self.socket_addr, slave).await
    }
}

#[derive(Debug)]
pub struct RobustClient<C: Connector> {
    connector: C,
    client: Option<C::Output>,
    slave: Slave,
}

impl<C: Connector> RobustClient<C> {
    pub fn new(connector: C, slave: Slave) -> Self {
        Self {
            connector,
            client: None,
            slave,
        }
    }
}

pub fn new_sync<T: Client, F: Fn(Slave) -> Result<T, Error> + Send + Sync>(
    factory: F,
    slave: Slave,
) -> RobustClient<SyncConnector<T, F>> {
    RobustClient::new(SyncConnector::new(factory), slave)
}

pub fn new_tcp_slave(socket_addr: SocketAddr, slave: Slave) -> RobustClient<TcpSlaveConnector> {
    RobustClient::new(TcpSlaveConnector::new(socket_addr), slave)
}

pub fn new_rtu_slave(
    device: impl Into<String>,
    baud_rate: u32,
    slave: Slave,
) -> RobustClient<SyncConnector<Context, impl Fn(Slave) -> Result<Context, Error> + Send + Sync>> {
    let device = device.into();
    new_sync(
        move |slave| -> Result<Context, Error> {
            let serial_builder = tokio_serial::new(&device, baud_rate);
            let serial_stream = tokio_serial::SerialStream::open(&serial_builder)?;
            Ok(tokio_modbus::client::rtu::attach_slave(
                serial_stream,
                slave,
            ))
        },
        slave,
    )
}

impl<C: Connector> SlaveContext for RobustClient<C> {
    fn set_slave(&mut self, slave: Slave) {
        self.slave = slave;
        if let Some(ref mut client) = self.client {
            client.set_slave(slave)
        }
    }
}

#[async_trait]
impl<C: Connector> Client for RobustClient<C> {
    async fn call(&mut self, req: Request<'_>) -> Result<Response, Error> {
        let client = match self.client {
            None => {
                let c = self.connector.connect(self.slave).await?;
                self.client.insert(c)
            }
            Some(ref mut c) => c,
        };
        // TODO: need to put the disconnect/retry logic in here
        client.call(req).await
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio_modbus::prelude::*;

    #[derive(Debug)]
    struct DummyConnector<I: Iterator<Item = Result<Response, Error>> + Send + Debug> {
        data: Arc<Mutex<I>>,
    }

    #[derive(Debug)]
    struct DummyClient<I: Iterator<Item = Result<Response, Error>> + Send + Debug> {
        data: Arc<Mutex<I>>,
    }

    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> DummyConnector<I> {
        fn new(data: I) -> Self {
            Self {
                data: Arc::new(Mutex::new(data)),
            }
        }
    }

    #[async_trait]
    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> Connector for DummyConnector<I> {
        type Output = DummyClient<I>;

        async fn connect(&self, _slave: Slave) -> Result<DummyClient<I>, Error> {
            Ok(DummyClient {
                data: self.data.clone(),
            })
        }
    }

    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> SlaveContext for DummyClient<I> {
        fn set_slave(&mut self, _slave: Slave) {}
    }

    #[async_trait]
    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> Client for DummyClient<I> {
        async fn call(&mut self, _req: Request<'_>) -> Result<Response, Error> {
            self.data.lock().unwrap().next().unwrap()
        }
    }

    #[tokio::test]
    async fn test_success() {
        let responses = vec![Ok(Response::ReadHoldingRegisters(vec![123]))].into_iter();
        let client = RobustClient::new(DummyConnector::new(responses), Slave(1));
        let client = Box::new(client);
        let client = client as Box<dyn Client>;
        let mut client: Context = client.into();
        let result = client.read_holding_registers(321, 1).await.unwrap();
        assert_eq!(result, vec![123]);
    }
}
