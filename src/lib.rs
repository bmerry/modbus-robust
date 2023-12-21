use async_trait::async_trait;
use std::fmt::Debug;
use std::io::Error;
use std::net::SocketAddr;
use tokio_modbus::{Request, Response};
use tokio_modbus::slave::{Slave, SlaveContext};
use tokio_modbus::client::{Client, Context};

#[async_trait]
pub trait Connector<T: Client> : Send + Debug {
    async fn connect(&self) -> Result<T, Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcpSlaveConnector {
    socket_addr: SocketAddr,
    slave: Slave,
}

impl TcpSlaveConnector {
    pub fn new(socket_addr: SocketAddr, slave: Slave) -> Self {
        Self { socket_addr, slave }
    }
}

#[async_trait]
impl Connector<Context> for TcpSlaveConnector {
    async fn connect(&self) -> Result<Context, Error> {
        tokio_modbus::client::tcp::connect_slave(self.socket_addr, self.slave).await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtuSlaveConnector {
    device: String,
    baud_rate: u32,
    slave: Slave,
}

impl RtuSlaveConnector {
    pub fn new(path: impl Into<String>, baud_rate: u32, slave: Slave) -> Self {
        Self { device: path.into(), baud_rate, slave }
    }
}

#[async_trait]
impl Connector<Context> for RtuSlaveConnector {
    async fn connect(&self) -> Result<Context, Error> {
        let serial_builder = tokio_serial::new(&self.device, self.baud_rate);
        let serial_stream = tokio_serial::SerialStream::open(&serial_builder)?;
        Ok(tokio_modbus::client::rtu::attach_slave(serial_stream, self.slave))
    }
}

#[derive(Debug)]
pub struct RobustClient<T: Client> {
    connector: Box<dyn Connector<T>>,
    conn: Option<T>,
    slave: Slave,
}

impl<T: Client> RobustClient<T> {
    pub fn new(connector: impl Connector<T> + 'static) -> Self {
        Self { connector: Box::new(connector), conn: None, slave: Slave(0) }
    }
}

impl RobustClient<Context> {
    pub fn new_tcp_slave(socket_addr: SocketAddr, slave: Slave) -> Self {
        Self::new(TcpSlaveConnector::new(socket_addr, slave))
    }

    pub fn new_rtu_slave(device: impl Into<String>, baud_rate: u32, slave: Slave) -> Self {
        Self::new(RtuSlaveConnector::new(device, baud_rate, slave))
    }
}

impl<T: Client> SlaveContext for RobustClient<T> {
    fn set_slave(&mut self, slave: Slave) {
        self.slave = slave;
        if let Some(ref mut conn) = self.conn {
            conn.set_slave(slave);
        }
    }
}

#[async_trait]
impl<T: Client> Client for RobustClient<T> {
    async fn call(&mut self, req: Request<'_>) -> Result<Response, Error> {
        if self.conn.is_none() {
            self.conn = Some(self.connector.connect().await?);
        }
        self.conn.as_mut().unwrap().call(req).await
    }
}
