//! Bindings for Unix Domain Sockets and futures
//!
//! This crate provides bindings between `mio_uds`, the mio crate for Unix
//! Domain sockets, and `futures`. The APIs and bindings in this crate are very
//! similar to the TCP and UDP bindings in the `futures-mio` crate. This crate
//! is also an empty crate on Windows, as Unix Domain Sockets are Unix-specific.

// NB: this is all *very* similar to TCP/UDP, and that's intentional!

#![cfg(unix)]
#![deny(missing_docs)]

extern crate futures;
extern crate futures_io;
extern crate futures_mio;
extern crate mio_uds;
#[macro_use]
extern crate log;

use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::mem;
use std::net::Shutdown;
use std::os::unix::net::SocketAddr;
use std::os::unix::prelude::*;
use std::path::Path;

use futures::stream::Stream;
use futures::{Future, Poll};
use futures_io::{IoFuture, IoStream};
use futures_mio::{ReadinessStream, LoopHandle};

/// A Unix socket which can accept connections from other unix sockets.
pub struct UnixListener {
    io: ReadinessStream<mio_uds::UnixListener>,
}

impl UnixListener {
    /// Creates a new `UnixListener` bound to the specified path.
    pub fn bind<P>(path: P, handle: LoopHandle) -> IoFuture<UnixListener>
        where P: AsRef<Path>
    {
        UnixListener::_bind(path.as_ref(), handle)
    }

    fn _bind(path: &Path, handle: LoopHandle) -> IoFuture<UnixListener> {
        match mio_uds::UnixListener::bind(path) {
            Ok(s) => UnixListener::new(s, handle),
            Err(e) => futures::failed(e).boxed(),
        }
    }

    fn new(listener: mio_uds::UnixListener,
           handle: LoopHandle) -> IoFuture<UnixListener> {
        ReadinessStream::new(handle, listener).map(|io| {
            UnixListener { io: io }
        }).boxed()
    }

    /// Returns the local socket address of this listener.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().local_addr()
    }

    /// Returns the value of the `SO_ERROR` option.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.io.get_ref().take_error()
    }

    /// Consumes this listener, returning a stream of the sockets this listener
    /// accepts.
    ///
    /// This method returns an implementation of the `Stream` trait which
    /// resolves to the sockets the are accepted on this listener.
    pub fn incoming(self) -> IoStream<(UnixStream, SocketAddr)> {
        struct Incoming {
            inner: UnixListener,
        }

        impl Stream for Incoming {
            type Item = (mio_uds::UnixStream, SocketAddr);
            type Error = io::Error;

            fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
                match self.inner.io.get_ref().accept() {
                    Ok(Some(pair)) => {
                        Poll::Ok(Some(pair))
                    }
                    Ok(None) => {
                        self.inner.io.need_read();
                        Poll::NotReady
                    }
                    Err(e) => Poll::Err(e),
                }
            }
        }

        let loop_handle = self.io.loop_handle().clone();
        Incoming { inner: self }
            .and_then(move |(client, addr)| {
                ReadinessStream::new(loop_handle.clone(), client).map(move |io| {
                    (UnixStream { io: io }, addr)
                })
            }).boxed()
    }
}

impl fmt::Debug for UnixListener {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.io.get_ref().fmt(f)
    }
}

impl AsRawFd for UnixListener {
    fn as_raw_fd(&self) -> RawFd {
        self.io.get_ref().as_raw_fd()
    }
}

/// A structure representing a connected unix socket.
///
/// This socket can be connected directly with `UnixStream::connect` or accepted
/// from a listener with `UnixListener::incoming`. Additionally, a pair of
/// anonymous Unix sockets can be created with `UnixStream::pair`.
pub struct UnixStream {
    io: ReadinessStream<mio_uds::UnixStream>,
}

enum UnixStreamNew {
    Waiting(UnixStream),
    Empty,
}

impl UnixStream {
    /// Connects to the socket named by `path`.
    ///
    /// This function will create a new unix socket and connect to the path
    /// specified, performing associating the returned stream with the provided
    /// event loop's handle.
    ///
    /// The returned future will resolve once the stream is successfully
    /// connected.
    pub fn connect<P>(p: P, handle: LoopHandle) -> IoFuture<UnixStream>
        where P: AsRef<Path>
    {
        UnixStream::_connect(p.as_ref(), handle)
    }

    fn _connect(path: &Path, handle: LoopHandle) -> IoFuture<UnixStream> {
        match mio_uds::UnixStream::connect(path) {
            Ok(s) => UnixStream::new(s, handle),
            Err(e) => futures::failed(e).boxed(),
        }
    }

    /// Creates an unnamed pair of connected sockets.
    ///
    /// This function will create a pair of interconnected unix sockets for
    /// communicating back and forth between one another. Each socket will be
    /// associated with the event loop whose handle is also provided.
    pub fn pair(handle: LoopHandle) -> IoFuture<(UnixStream, UnixStream)> {
        match mio_uds::UnixStream::pair() {
            Ok((a, b)) => {
                let a = UnixStream::new(a, handle.clone());
                let b = UnixStream::new(b, handle.clone());
                a.join(b).boxed()
            }
            Err(e) => futures::failed(e).boxed(),
        }
    }

    fn new(stream: mio_uds::UnixStream, handle: LoopHandle)
           -> IoFuture<UnixStream> {
        ReadinessStream::new(handle, stream).and_then(|io| {
            UnixStreamNew::Waiting(UnixStream { io: io })
        }).boxed()
    }

    /// Test whether this socket is ready to be read or not.
    pub fn poll_read(&self) -> Poll<(), io::Error> {
        self.io.poll_read()
    }

    /// Test whether this socket is writey to be written to or not.
    pub fn poll_write(&self) -> Poll<(), io::Error> {
        self.io.poll_write()
    }

    /// Returns the socket address of the local half of this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().local_addr()
    }

    /// Returns the socket address of the remote half of this connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().peer_addr()
    }

    /// Returns the value of the `SO_ERROR` option.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.io.get_ref().take_error()
    }

    /// Shuts down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O calls on the
    /// specified portions to immediately return with an appropriate value
    /// (see the documentation of `Shutdown`).
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.io.get_ref().shutdown(how)
    }
}

impl Future for UnixStreamNew {
    type Item = UnixStream;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<UnixStream, io::Error> {
        let stream = match mem::replace(self, UnixStreamNew::Empty) {
            UnixStreamNew::Waiting(s) => s,
            UnixStreamNew::Empty => panic!("can't poll Unix stream twice"),
        };
        match stream.io.poll_write() {
            Poll::Ok(()) => {
                match stream.io.get_ref().take_error() {
                    Ok(None) => return Poll::Ok(stream),
                    Ok(Some(e)) => return Poll::Err(e),
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) => return Poll::Err(e),
                }
            }
            Poll::Err(e) => return Poll::Err(e),
            Poll::NotReady => {}
        }
        *self = UnixStreamNew::Waiting(stream);
        Poll::NotReady
    }
}

impl Read for UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let r = self.io.get_ref().read(buf);
        if is_wouldblock(&r) {
            self.io.need_read();
        }
        return r
    }
}

impl Write for UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let r = self.io.get_ref().write(buf);
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }

    fn flush(&mut self) -> io::Result<()> {
        let r = self.io.get_ref().flush();
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }
}

impl<'a> Read for &'a UnixStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let r = self.io.get_ref().read(buf);
        if is_wouldblock(&r) {
            self.io.need_read();
        }
        return r
    }
}

impl<'a> Write for &'a UnixStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let r = self.io.get_ref().write(buf);
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }

    fn flush(&mut self) -> io::Result<()> {
        let r = self.io.get_ref().flush();
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }
}

impl fmt::Debug for UnixStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.io.get_ref().fmt(f)
    }
}

impl AsRawFd for UnixStream {
    fn as_raw_fd(&self) -> RawFd {
        self.io.get_ref().as_raw_fd()
    }
}

/// An I/O object representing a Unix datagram socket.
pub struct UnixDatagram {
    io: ReadinessStream<mio_uds::UnixDatagram>,
}

impl UnixDatagram {
    /// Creates a new `UnixDatagram` bound to the specified path.
    pub fn bind<P>(path: P, handle: LoopHandle) -> IoFuture<UnixDatagram>
        where P: AsRef<Path>
    {
        UnixDatagram::_bind(path.as_ref(), handle)
    }

    fn _bind(path: &Path, handle: LoopHandle) -> IoFuture<UnixDatagram> {
        match mio_uds::UnixDatagram::bind(path) {
            Ok(s) => UnixDatagram::new(s, handle),
            Err(e) => futures::failed(e).boxed(),
        }
    }

    /// Creates an unnamed pair of connected sockets.
    ///
    /// This function will create a pair of interconnected unix sockets for
    /// communicating back and forth between one another. Each socket will be
    /// associated with the event loop whose handle is also provided.
    pub fn pair(handle: LoopHandle) -> IoFuture<(UnixDatagram, UnixDatagram)> {
        match mio_uds::UnixDatagram::pair() {
            Ok((a, b)) => {
                let a = UnixDatagram::new(a, handle.clone());
                let b = UnixDatagram::new(b, handle.clone());
                a.join(b).boxed()
            }
            Err(e) => futures::failed(e).boxed(),
        }
    }


    fn new(socket: mio_uds::UnixDatagram, handle: LoopHandle)
           -> IoFuture<UnixDatagram> {
        ReadinessStream::new(handle, socket).map(|io| {
            UnixDatagram { io: io }
        }).boxed()
    }

    /// Connects the socket to the specified address.
    ///
    /// The `send` method may be used to send data to the specified address.
    /// `recv` and `recv_from` will only receive data from that address.
    pub fn connect<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        self.io.get_ref().connect(path)
    }

    /// Test whether this socket is ready to be read or not.
    pub fn poll_read(&self) -> Poll<(), io::Error> {
        self.io.poll_read()
    }

    /// Test whether this socket is writey to be written to or not.
    pub fn poll_write(&self) -> Poll<(), io::Error> {
        self.io.poll_write()
    }

    /// Returns the local address that this socket is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().local_addr()
    }

    /// Returns the address of this socket's peer.
    ///
    /// The `connect` method will connect the socket to a peer.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.io.get_ref().peer_addr()
    }

    /// Receives data from the socket.
    ///
    /// On success, returns the number of bytes read and the address from
    /// whence the data came.
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let r = self.io.get_ref().recv_from(buf);
        if is_wouldblock(&r) {
            self.io.need_read();
        }
        return r
    }

    /// Receives data from the socket.
    ///
    /// On success, returns the number of bytes read.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let r = self.io.get_ref().recv(buf);
        if is_wouldblock(&r) {
            self.io.need_read();
        }
        return r
    }

    /// Sends data on the socket to the specified address.
    ///
    /// On success, returns the number of bytes written.
    pub fn send_to<P>(&self, buf: &[u8], path: P) -> io::Result<usize>
        where P: AsRef<Path>
    {
        let r = self.io.get_ref().send_to(buf, path);
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }

    /// Sends data on the socket to the socket's peer.
    ///
    /// The peer address may be set by the `connect` method, and this method
    /// will return an error if the socket has not already been connected.
    ///
    /// On success, returns the number of bytes written.
    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let r = self.io.get_ref().send(buf);
        if is_wouldblock(&r) {
            self.io.need_write();
        }
        return r
    }

    /// Returns the value of the `SO_ERROR` option.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.io.get_ref().take_error()
    }

    /// Shut down the read, write, or both halves of this connection.
    ///
    /// This function will cause all pending and future I/O calls on the
    /// specified portions to immediately return with an appropriate value
    /// (see the documentation of `Shutdown`).
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.io.get_ref().shutdown(how)
    }
}

impl fmt::Debug for UnixDatagram {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.io.get_ref().fmt(f)
    }
}

impl AsRawFd for UnixDatagram {
    fn as_raw_fd(&self) -> RawFd {
        self.io.get_ref().as_raw_fd()
    }
}

fn is_wouldblock<T>(r: &io::Result<T>) -> bool {
    match *r {
        Ok(_) => false,
        Err(ref e) => e.kind() == io::ErrorKind::WouldBlock,
    }
}
