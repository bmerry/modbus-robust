/* Copyright 2022-2023 Bruce Merry
 *
 * This program is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation, either version 3 of the License, or (at your option)
 * any later version.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 * more details.
 *
 * You should have received a copy of the GNU General Public License along
 * with this program. If not, see <https://www.gnu.org/licenses/>.
 */

#![doc = include_str!("../README.md")]

use async_trait::async_trait;
use std::fmt::Debug;
use std::io::Error;
use std::net::SocketAddr;
use tokio_modbus::client::{Client, Context};
use tokio_modbus::slave::{Slave, SlaveContext};
use tokio_modbus::{Request, Response};

/// Establish a connection. The implementation must support calling
/// [`Connector::connect`] multiple times.
#[async_trait]
pub trait Connector: Send + Debug {
    type Output: Client;

    /// Establish a connection.
    async fn connect(&mut self, slave: Slave) -> Result<Self::Output, Error>;
}

/// Establish a connection using a factory function.
///
/// In practice, the function needs to be `'static` to be able to use this to
/// obtain a [`tokio_modbus::client::Context`].
pub struct SyncConnector<T: Client, F: FnMut(Slave) -> Result<T, Error> + Send + Sync> {
    factory: F,
}

impl<T: Client, F: FnMut(Slave) -> Result<T, Error> + Send + Sync> SyncConnector<T, F> {
    /// Create from a factory function.
    pub fn new(factory: F) -> Self {
        Self { factory }
    }
}

impl<T: Client, F: FnMut(Slave) -> Result<T, Error> + Send + Sync> Debug for SyncConnector<T, F> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "SyncConnector()")
    }
}

#[async_trait]
impl<T: Client, F: FnMut(Slave) -> Result<T, Error> + Send + Sync> Connector
    for SyncConnector<T, F>
{
    type Output = T;

    async fn connect(&mut self, slave: Slave) -> Result<T, Error> {
        (self.factory)(slave)
    }
}

/// Implementation of [`Connector`] for TCP connections.
#[derive(Debug)]
pub struct TcpSlaveConnector {
    socket_addr: SocketAddr,
}

impl TcpSlaveConnector {
    /// Construct.
    pub fn new(socket_addr: SocketAddr) -> Self {
        Self { socket_addr }
    }
}

#[async_trait]
impl Connector for TcpSlaveConnector {
    type Output = Context;

    async fn connect(&mut self, slave: Slave) -> Result<Context, Error> {
        tokio_modbus::client::tcp::connect_slave(self.socket_addr, slave).await
    }
}

/// Client that automatically reconnects and retries on failure.
#[derive(Debug)]
pub struct RobustClient<C: Connector> {
    connector: C,
    client: Option<C::Output>,
    slave: Slave,
}

impl<C: Connector> RobustClient<C> {
    /// Construct a robust client.
    ///
    /// When constructed, there is no established connection. An attempt will
    /// be made to establish one on the first call.
    pub fn new(connector: C, slave: Slave) -> Self {
        Self {
            connector,
            client: None,
            slave,
        }
    }
}

impl<C: Connector + 'static> RobustClient<C> {
    /// Construct a robust client wrapped in a
    /// [`tokio_modbus::client::Context`].
    ///
    /// This is the constructor you will most likely want to use, because
    /// [Context][`tokio_modbus::client::Context`] provides all the
    /// convenience functions.
    pub fn new_context(connector: C, slave: Slave) -> Context {
        (Box::new(Self::new(connector, slave)) as Box<dyn Client>).into()
    }
}

/// Construct a [`tokio_modbus::client::Context`] from a connection factory
/// function.
///
/// The connection is not immediately established. It will be attempted on
/// the first call.
pub fn new_sync<
    T: Client + 'static,
    F: FnMut(Slave) -> Result<T, Error> + Send + Sync + 'static,
>(
    factory: F,
    slave: Slave,
) -> Context {
    RobustClient::new_context(SyncConnector::new(factory), slave)
}

/// Construct a [`tokio_modbus::client::Context`] for a TCP connection.
///
/// The connection is not immediately established. It will be attempted on
/// the first call.
pub fn new_tcp_slave(socket_addr: SocketAddr, slave: Slave) -> Context {
    RobustClient::new_context(TcpSlaveConnector::new(socket_addr), slave)
}

/// Construct a [`tokio_modbus::client::Context`] for an RTU connection.
///
/// The connection is not immediately established. It will be attempted on
/// the first call.
///
/// This implementation only allows the baud rate to be set, and not other
/// options such as the parity bits. If more control is needed, use
/// [`new_sync`].
pub fn new_rtu_slave(device: impl Into<String>, baud_rate: u32, slave: Slave) -> Context {
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
        let (client, fresh) = match self.client {
            None => {
                let c = self.connector.connect(self.slave).await?;
                (self.client.insert(c), true)
            }
            Some(ref mut c) => (c, false),
        };
        match client.call(req.clone()).await {
            result if fresh => result, // Don't retry if this is a brand new connection
            Ok(response) => Ok(response),
            Err(_) => {
                let c = self.connector.connect(self.slave).await?;
                self.client.insert(c).call(req).await
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio_modbus::prelude::*;

    trait DummyState: Send + Debug {
        fn connect(&mut self, slave: Slave) -> Result<(), Error>;
        fn call(&mut self, req: Request) -> Result<Response, Error>;
    }

    #[derive(Debug)]
    struct IterDummyState<
        I: Iterator<Item = Result<Response, Error>> + Send + Debug,
        J: Iterator<Item = Result<(), Error>> + Send + Debug,
    > {
        responses: I,
        connects: J,
    }

    impl<
            I: Iterator<Item = Result<Response, Error>> + Send + Debug,
            J: Iterator<Item = Result<(), Error>> + Send + Debug,
        > IterDummyState<I, J>
    {
        fn new(responses: I, connects: J) -> Self {
            Self {
                responses,
                connects,
            }
        }
    }

    impl<
            I: Iterator<Item = Result<Response, Error>> + Send + Debug,
            J: Iterator<Item = Result<(), Error>> + Send + Debug,
        > DummyState for IterDummyState<I, J>
    {
        fn connect(&mut self, _slave: Slave) -> Result<(), Error> {
            self.connects.next().unwrap()
        }

        fn call(&mut self, _req: Request) -> Result<Response, Error> {
            self.responses.next().unwrap()
        }
    }

    #[derive(Debug)]
    struct DummyConnector<S: DummyState> {
        state: Arc<Mutex<S>>,
    }

    #[derive(Debug)]
    struct DummyClient<S: DummyState> {
        state: Arc<Mutex<S>>,
    }

    impl<S: DummyState> DummyConnector<S> {
        fn new(state: S) -> Self {
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }
    }

    #[async_trait]
    impl<S: DummyState> Connector for DummyConnector<S> {
        type Output = DummyClient<S>;

        async fn connect(&mut self, slave: Slave) -> Result<DummyClient<S>, Error> {
            let mut state = self.state.lock().unwrap();
            state.connect(slave).map(|_| DummyClient {
                state: self.state.clone(),
            })
        }
    }

    impl<S: DummyState> SlaveContext for DummyClient<S> {
        fn set_slave(&mut self, _slave: Slave) {}
    }

    #[async_trait]
    impl<S: DummyState> Client for DummyClient<S> {
        async fn call(&mut self, req: Request<'_>) -> Result<Response, Error> {
            let mut state = self.state.lock().unwrap();
            state.call(req)
        }
    }

    fn make_client_always_connect(responses: Vec<Result<Response, Error>>) -> Context {
        let state = IterDummyState::new(responses.into_iter(), std::iter::repeat_with(|| Ok(())));
        RobustClient::new_context(DummyConnector::new(state), Slave(1))
    }

    fn make_client(
        responses: Vec<Result<Response, Error>>,
        connects: Vec<Result<(), Error>>,
    ) -> Context {
        let state = IterDummyState::new(responses.into_iter(), connects.into_iter());
        RobustClient::new_context(DummyConnector::new(state), Slave(1))
    }

    #[tokio::test]
    async fn test_success() {
        let responses = vec![Ok(Response::ReadHoldingRegisters(vec![123]))];
        let mut client = make_client_always_connect(responses);
        let result = client.read_holding_registers(321, 1).await.unwrap();
        assert_eq!(result, vec![123]);
    }

    #[tokio::test]
    async fn test_call_failure() {
        let responses = vec![
            Ok(Response::ReadHoldingRegisters(vec![123])),
            Err(Error::from(std::io::ErrorKind::ConnectionReset)),
            Ok(Response::ReadHoldingRegisters(vec![123])),
        ];
        let mut client = make_client_always_connect(responses);
        let _ = client.read_holding_registers(321, 1).await; // Establish connection
        let result = client.read_holding_registers(321, 1).await.unwrap();
        assert_eq!(result, vec![123]);
    }

    #[tokio::test]
    async fn test_call_double_failure() {
        let responses = vec![
            Ok(Response::ReadHoldingRegisters(vec![123])),
            Err(Error::from(std::io::ErrorKind::ConnectionReset)),
            Err(Error::from(std::io::ErrorKind::PermissionDenied)),
        ];
        let mut client = make_client_always_connect(responses);
        let _ = client.read_holding_registers(321, 1).await; // Establish connection
        let result = client.read_holding_registers(321, 1).await.unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn test_connect_failure() {
        let responses = vec![];
        let connects = vec![Err(Error::from(std::io::ErrorKind::ConnectionRefused))];
        let mut client = make_client(responses, connects);
        let result = client.read_holding_registers(321, 1).await.unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn test_connect_failure2() {
        let responses = vec![
            Ok(Response::ReadHoldingRegisters(vec![123])),
            Err(Error::from(std::io::ErrorKind::ConnectionReset)),
        ];
        let connects = vec![
            Ok(()),
            Err(Error::from(std::io::ErrorKind::ConnectionRefused)),
        ];
        let mut client = make_client(responses, connects);
        let _ = client.read_holding_registers(321, 1).await; // Establish connection
        let result = client.read_holding_registers(321, 1).await.unwrap_err();
        assert_eq!(result.kind(), std::io::ErrorKind::ConnectionRefused);
    }
}
