use async_trait::async_trait;
use std::fmt::Debug;
use std::io::Error;
use std::net::SocketAddr;
use tokio_modbus::client::{Client, Context};
use tokio_modbus::slave::{Slave, SlaveContext};
use tokio_modbus::{Request, Response};

#[async_trait]
pub trait Connector: SlaveContext + Send + Debug {
    type Output: Client;

    async fn get_client(&mut self) -> Result<&mut Self::Output, Error>;
    fn disconnect(&mut self);
}

pub struct SyncConnector<T: Client, F: Fn(Slave) -> Result<T, Error> + Send> {
    factory: F,
    client: Option<T>,
    slave: Slave,
}

impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send> SyncConnector<T, F> {
    pub fn new(factory: F, slave: Slave) -> Self {
        Self { factory, client: None, slave }
    }
}

impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send> Debug for SyncConnector<T, F> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "SyncConnector<client = {:?}, slave = {:?}>", self.client, self.slave)
    }
}

impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send> SlaveContext for SyncConnector<T, F>
{
    fn set_slave(&mut self, slave: Slave) {
        self.slave = slave;
        if let Some(client) = &mut self.client {
            client.set_slave(slave)
        }
    }
}

#[async_trait]
impl<T: Client, F: Fn(Slave) -> Result<T, Error> + Send> Connector
    for SyncConnector<T, F>
{
    type Output = T;

    async fn get_client(&mut self) -> Result<&mut T, Error> {
        match self.client {
            None => {
                let client = (self.factory)(self.slave)?;
                Ok(self.client.insert(client))
            },
            Some(ref mut client) => Ok(client)
        }
    }

    fn disconnect(&mut self) {
        self.client = None;
    }
}

#[derive(Debug)]
pub struct TcpSlaveConnector {
    socket_addr: SocketAddr,
    client: Option<Context>,
    slave: Slave,
}

impl TcpSlaveConnector {
    pub fn new(socket_addr: SocketAddr, slave: Slave) -> Self {
        Self { socket_addr, client: None, slave }
    }
}

impl SlaveContext for TcpSlaveConnector {
    fn set_slave(&mut self, slave: Slave) {
        self.slave = slave;
        if let Some(ref mut client) = self.client {
            client.set_slave(slave);
        }
    }
}

#[async_trait]
impl Connector for TcpSlaveConnector {
    type Output = Context;

    async fn get_client(&mut self) -> Result<&mut Context, Error> {
        match self.client {
            None => {
                let client = tokio_modbus::client::tcp::connect_slave(self.socket_addr, self.slave).await?;
                Ok(self.client.insert(client))
            },
            Some(ref mut client) => Ok(client)
        }
    }

    fn disconnect(&mut self) {
        self.client = None;
    }
}

#[derive(Debug)]
pub struct RobustClient<C: Connector> {
    connector: C,
}

impl<C: Connector> RobustClient<C> {
    pub fn new(connector: C) -> Self {
        Self { connector }
    }
}

pub fn new_sync<T: Client, F: Fn(Slave) -> Result<T, Error> + Send>(
    factory: F,
    slave: Slave,
) -> RobustClient<SyncConnector<T, F>> {
    RobustClient::new(SyncConnector::new(factory, slave))
}

pub fn new_tcp_slave(socket_addr: SocketAddr, slave: Slave) -> RobustClient<TcpSlaveConnector> {
    RobustClient::new(TcpSlaveConnector::new(socket_addr, slave))
}

pub fn new_rtu_slave(device: impl Into<String>, baud_rate: u32, slave: Slave) -> RobustClient<SyncConnector<Context, impl Fn(Slave) -> Result<Context, Error> + Send>> {
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
        self.connector.set_slave(slave)
    }
}

#[async_trait]
impl<C: Connector> Client for RobustClient<C> {
    async fn call(&mut self, req: Request<'_>) -> Result<Response, Error> {
        let client = self.connector.get_client().await?;
        // TODO: need to put the disconnect/retry logic in here
        client.call(req).await
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tokio_modbus::prelude::*;

    #[derive(Debug)]
    struct DummyConnector<I: Iterator<Item = Result<Response, Error>> + Send + Debug> {
        data: I,
        slave: Slave
    }

    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> DummyConnector<I> {
        fn new(data: I, slave: Slave) -> Self {
            Self { data, slave }
        }
    }

    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> SlaveContext for DummyConnector<I> {
        fn set_slave(&mut self, slave: Slave) {
            self.slave = slave;
        }
    }

    #[async_trait]
    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> Connector for DummyConnector<I> {
        type Output = Self;

        async fn get_client(&mut self) -> Result<&mut Self, Error> {
            Ok(self)
        }

        fn disconnect(&mut self) {
        }
    }

    #[async_trait]
    impl<I: Iterator<Item = Result<Response, Error>> + Send + Debug> Client for DummyConnector<I> {
        async fn call(&mut self, _req: Request<'_>) -> Result<Response, Error> {
            self.data.next().unwrap()
        }
    }

    #[tokio::test]
    async fn test_success() {
        let responses = vec![Ok(Response::ReadHoldingRegisters(vec![123]))].into_iter();
        let client = RobustClient::new(DummyConnector::new(responses, Slave(1)));
        let client = Box::new(client);
        let client = client as Box<dyn Client>;
        let mut client: Context = client.into();
        let result = client.read_holding_registers(321, 1).await.unwrap();
        assert_eq!(result, vec![123]);
    }
}
