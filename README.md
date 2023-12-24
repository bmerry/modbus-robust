# Make tokio-modbus more robust to connection loss

This library is a layer on top of
[tokio-modbus](https://docs.rs/tokio-modbus/latest/tokio_modbus/index.html).
When a modbus call fails, it will automatically discard the current connection
and reconnect. This makes it robust to TCP connection failures, such as can be
caused by restarting `mbusd`.

This is not a complete retry mechanism. For example, if reconnecting fails,
the call will fail rather than making further attempts to connect. If multiple
retries are desired, they can be layered on top of this library, without
needing to worry about re-establishing failed connections as each call will
automatically attempt to do so.

It also does not implement any form of timeout; again, this should be layered
on top.

Finally, note that it is possible that a call may actually be received twice
by the modbus device. For example, it could be that the command was
successfully received by the reply failed to be delivered. This library should
thus only be used if the calls are idempotent.

## Usage

For simple use cases, use either [`new_tcp_slave`] for TCP/IP or
[`new_rtu_slave`] for serial. The latter is a shortcut for cases where only
the device path and baud rate need to be set for the serial port. If other
options (such as parity) need to be changed, use [`new_sync`]: it takes a
factory function that produces a connection. It only works for synchronous
connection functions.

For more advanced use cases, implement the [`SyncConnector`] trait to specify
how to establish a connection (asynchronously). Then pass the implementation
to [`RobustClient::new_context`].
