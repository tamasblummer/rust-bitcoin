// Rust Bitcoin Library
// Written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! Stream reader
//!
//! This module defines `StreamReader` struct and its implementation which is used
//! for parsing incoming stream into separate `RawNetworkMessage`s, handling assembling
//! messages from multiple packets or dealing with partial or multiple messages in the stream
//! (like can happen with reading from TCP socket)
//!

use std::fmt;
use std::io;
use std::io::Read;
use std::sync::mpsc::Sender;

use util;
use network::message::{NetworkMessage, RawNetworkMessage};
use consensus::encode;

/// A response from the peer-connected socket
pub enum SocketResponse {
    /// A message was received
    MessageReceived(NetworkMessage),
    /// An error occurred and the socket needs to close
    ConnectionFailed(util::Error, Sender<()>)
}

/// Struct used to configure stream reader function
pub struct StreamReader<'a> {
    /// Size of allocated buffer for a single read opetaion
    pub buffer_size: usize,
    /// Stream to read from
    pub stream: &'a mut Read,
    /// Buffer containing unparsed message part
    unparsed: Vec<u8>
}

impl<'a> fmt::Debug for StreamReader<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "StreamReader with buffer_size={} and unparsed content {:?}",
               self.buffer_size, self.unparsed)
    }
}

impl<'a> StreamReader<'a> {
    /// Constructs new stream reader for a given input stream `stream` with
    /// optional parameter `buffer_size` determining reading buffer size
    pub fn new(stream: &mut Read, buffer_size: Option<usize>) -> StreamReader {
        StreamReader {
            stream,
            buffer_size: buffer_size.unwrap_or(64 * 1024),
            unparsed: vec![]
        }
    }

    /// Reads stream and parses messages from its current input,
    /// also taking into account previously unparsed partial message (if there was such).
    ///
    /// ## Note:
    /// The reason why the function returns an array of messages instead of a single message
    /// is that Bitcoin protocol messages are distributed across TCP packets unevenly:
    /// one TCP packet can contain several messages (while other messages can be split into
    /// several TCP packets). Thus, if we will return just a single message per call,
    /// we will be locking the main process without returning all already delivered packages.
    pub fn read_messages(&mut self) -> Result<Vec<RawNetworkMessage>, encode::Error> {
        let mut messages: Vec<RawNetworkMessage> = vec![];
        let mut data = vec![0u8; self.buffer_size];

        // 4. Reiterating only if we were not able to parse even a single message, so we need
        //    to listen for more data from the stream (initially there is always zero messages)
        while messages.len() == 0 {
            // 1. First, we are waiting for a new network packet
            //    (even if we have some remaining parts in the self.unparsed buffer from last reads,
            //    we can't assemble a whole message from it, so we need to get some new data)
            let count = self.stream.read(&mut data)?;
            if count > 0 {
                self.unparsed.extend(data[0..count].iter());
            }
            // 2. Then we append it to the end of self.unparsed and parsing all the messages
            //    (there can be few of them in a single network packet) -
            //    this functionality is brought into a separate private fn `parse`
            messages.append(&mut self.parse()?);
        }
        // 3. We return all the messages we were able to assemble and do not wait for new packages
        //    from the network: the client needs to parse already received messages
        //    and when he will need new once he will simply call read_messages once again.
        return Ok(messages)
    }

    // Performs actual parsing of the block into separate messages (can be several within a
    // single block)
    fn parse(&mut self) -> Result<Vec<RawNetworkMessage>, encode::Error> {
        let mut messages: Vec<RawNetworkMessage> = vec![];
        while self.unparsed.len() > 0 {
            match encode::deserialize_partial::<RawNetworkMessage>(&self.unparsed) {
                // In this case we just have an incomplete data, so we need to read more
                Err(encode::Error::Io(ref err)) if err.kind() == io::ErrorKind::UnexpectedEof =>
                    return Ok(messages),
                // All other types of errors should be passed up to the caller
                Err(err) => return Err(err),
                // We have successfully read from the buffer
                Ok((message, index)) => {
                    messages.push(message);
                    self.unparsed.drain(..index);
                },
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod test {
    extern crate tempfile;

    use std::thread;
    use std::fs::File;
    use std::time::Duration;
    use std::io::{Write, Seek, SeekFrom};
    use std::net::{TcpListener, TcpStream, Shutdown};

    use super::StreamReader;
    use network::message::{NetworkMessage, RawNetworkMessage};

    const MSG_VERSION: [u8; 126] = [
        0xf9, 0xbe, 0xb4, 0xd9, 0x76, 0x65, 0x72, 0x73,
        0x69, 0x6f, 0x6e, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x66, 0x00, 0x00, 0x00, 0xbe, 0x61, 0xb8, 0x27,
        0x7f, 0x11, 0x01, 0x00, 0x0d, 0x04, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xf0, 0x0f, 0x4d, 0x5c,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff,
        0x5b, 0xf0, 0x8c, 0x80, 0xb4, 0xbd, 0x0d, 0x04,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xfa, 0xa9, 0x95, 0x59, 0xcc, 0x68, 0xa1, 0xc1,
        0x10, 0x2f, 0x53, 0x61, 0x74, 0x6f, 0x73, 0x68,
        0x69, 0x3a, 0x30, 0x2e, 0x31, 0x37, 0x2e, 0x31,
        0x2f, 0x93, 0x8c, 0x08, 0x00, 0x01
    ];

    const MSG_VERACK: [u8; 24] = [
        0xf9, 0xbe, 0xb4, 0xd9, 0x76, 0x65, 0x72, 0x61,
        0x63, 0x6b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x5d, 0xf6, 0xe0, 0xe2
    ];

    const MSG_PING: [u8; 32] = [
        0xf9, 0xbe, 0xb4, 0xd9, 0x70, 0x69, 0x6e, 0x67,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x00, 0x00, 0x24, 0x67, 0xf1, 0x1d,
        0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    const MSG_ALERT: [u8; 192] = [
        0xf9, 0xbe, 0xb4, 0xd9, 0x61, 0x6c, 0x65, 0x72,
        0x74, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xa8, 0x00, 0x00, 0x00, 0x1b, 0xf9, 0xaa, 0xea,
        0x60, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0x7f, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0x7f, 0xfe, 0xff, 0xff, 0x7f, 0x01, 0xff, 0xff,
        0xff, 0x7f, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x7f, 0x00, 0xff, 0xff, 0xff, 0x7f, 0x00,
        0x2f, 0x55, 0x52, 0x47, 0x45, 0x4e, 0x54, 0x3a,
        0x20, 0x41, 0x6c, 0x65, 0x72, 0x74, 0x20, 0x6b,
        0x65, 0x79, 0x20, 0x63, 0x6f, 0x6d, 0x70, 0x72,
        0x6f, 0x6d, 0x69, 0x73, 0x65, 0x64, 0x2c, 0x20,
        0x75, 0x70, 0x67, 0x72, 0x61, 0x64, 0x65, 0x20,
        0x72, 0x65, 0x71, 0x75, 0x69, 0x72, 0x65, 0x64,
        0x00, 0x46, 0x30, 0x44, 0x02, 0x20, 0x65, 0x3f,
        0xeb, 0xd6, 0x41, 0x0f, 0x47, 0x0f, 0x6b, 0xae,
        0x11, 0xca, 0xd1, 0x9c, 0x48, 0x41, 0x3b, 0xec,
        0xb1, 0xac, 0x2c, 0x17, 0xf9, 0x08, 0xfd, 0x0f,
        0xd5, 0x3b, 0xdc, 0x3a, 0xbd, 0x52, 0x02, 0x20,
        0x6d, 0x0e, 0x9c, 0x96, 0xfe, 0x88, 0xd4, 0xa0,
        0xf0, 0x1e, 0xd9, 0xde, 0xda, 0xe2, 0xb6, 0xf9,
        0xe0, 0x0d, 0xa9, 0x4c, 0xad, 0x0f, 0xec, 0xaa,
        0xe6, 0x6e, 0xcf, 0x68, 0x9b, 0xf7, 0x1b, 0x50
    ];

    fn check_version_msg(msg: &RawNetworkMessage) {
        assert_eq!(msg.magic, 0xd9b4bef9);
        if let NetworkMessage::Version(ref version_msg) = msg.payload {
            assert_eq!(version_msg.version, 70015);
            assert_eq!(version_msg.services, 1037);
            assert_eq!(version_msg.timestamp, 1548554224);
            assert_eq!(version_msg.nonce, 13952548347456104954);
            assert_eq!(version_msg.user_agent, "/Satoshi:0.17.1/");
            assert_eq!(version_msg.start_height, 560275);
            assert_eq!(version_msg.relay, true);
        } else {
            panic!("Wrong message type: expected VersionMessage");
        }
    }

    fn check_alert_msg(msg: &RawNetworkMessage) {
        assert_eq!(msg.magic, 0xd9b4bef9);
        if let NetworkMessage::Alert(ref alert) = msg.payload {
            assert_eq!(alert.clone(), [
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
                0x7f, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
                0x7f, 0xfe, 0xff, 0xff, 0x7f, 0x01, 0xff, 0xff,
                0xff, 0x7f, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff,
                0xff, 0x7f, 0x00, 0xff, 0xff, 0xff, 0x7f, 0x00,
                0x2f, 0x55, 0x52, 0x47, 0x45, 0x4e, 0x54, 0x3a,
                0x20, 0x41, 0x6c, 0x65, 0x72, 0x74, 0x20, 0x6b,
                0x65, 0x79, 0x20, 0x63, 0x6f, 0x6d, 0x70, 0x72,
                0x6f, 0x6d, 0x69, 0x73, 0x65, 0x64, 0x2c, 0x20,
                0x75, 0x70, 0x67, 0x72, 0x61, 0x64, 0x65, 0x20,
                0x72, 0x65, 0x71, 0x75, 0x69, 0x72, 0x65, 0x64,
                0x00,
            ].to_vec());
        } else {
            panic!("Wrong message type: expected AlertMessage");
        }
    }

    #[test]
    fn parse_multipartmsg_test() {
        let mut tmpfile: File = tempfile::tempfile().unwrap();
        let mut reader = StreamReader::new(&mut tmpfile, None);
        reader.unparsed = MSG_ALERT[..24].to_vec();
        let messages = reader.parse().unwrap();
        assert_eq!(messages.len(), 0);
        assert_eq!(reader.unparsed.len(), 24);

        reader.unparsed = MSG_ALERT.to_vec();
        let messages = reader.parse().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(reader.unparsed.len(), 0);

        check_alert_msg(messages.first().unwrap());
    }

    fn init_stream(buf: &[u8]) -> File {
        let mut tmpfile: File = tempfile::tempfile().unwrap();
        write_file(&mut tmpfile, &buf);
        tmpfile
    }

    fn write_file(tmpfile: &mut File, buf: &[u8]) {
        tmpfile.seek(SeekFrom::End(0)).unwrap();
        tmpfile.write(&buf).unwrap();
        tmpfile.flush().unwrap();
        tmpfile.seek(SeekFrom::Start(0)).unwrap();
    }

    #[test]
    fn read_singlemsg_test() {
        let mut stream = init_stream(&MSG_VERSION);
        let messages = StreamReader::new(&mut stream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 1);

        check_version_msg(messages.first().unwrap());
    }

    #[test]
    fn read_doublemsgs_test() {
        let mut stream = init_stream(&MSG_VERSION);
        write_file(&mut stream, &MSG_PING);

        let messages = StreamReader::new(&mut stream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 2);

        check_version_msg(messages.first().unwrap());

        let msg = messages.last().unwrap();
        assert_eq!(msg.magic, 0xd9b4bef9);
        if let NetworkMessage::Ping(nonce) = msg.payload {
            assert_eq!(nonce, 100);
        } else {
            panic!("Wrong message type, expected PingMessage");
        }
    }

    fn serve_tcp(port: u16, pieces: Vec<Vec<u8>>) {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).unwrap();
        for ostream in listener.incoming() {
            let mut ostream = ostream.unwrap();

            for piece in pieces {
                ostream.write(&piece[..]).unwrap();
                ostream.flush().unwrap();
                thread::sleep(Duration::from_secs(1));
            }

            ostream.shutdown(Shutdown::Both).unwrap();
            break;
        }
    }

    #[test]
    fn read_multipartmsg_test() {
        let port: u16 = 34254;
        let handle = thread::spawn(move || {
            serve_tcp(port, vec![MSG_VERSION[..24].to_vec(), MSG_VERSION[24..].to_vec()]);
        });

        thread::sleep(Duration::from_secs(1));
        let mut istream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        let messages = StreamReader::new(&mut istream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 1);

        let msg = messages.first().unwrap();
        check_version_msg(msg);

        handle.join().unwrap();
    }

    #[test]
    fn read_sequencemsg_test() {
        let port: u16 = 34255;
        let handle = thread::spawn(move || {
            serve_tcp(port, vec![
                // Real-world Bitcoin core communication case for /Satoshi:0.17.1/
                MSG_VERSION[..23].to_vec(), MSG_VERSION[23..].to_vec(),
                MSG_VERACK.to_vec(),
                MSG_ALERT[..24].to_vec(), MSG_ALERT[24..].to_vec()
            ]);
        });

        thread::sleep(Duration::from_secs(1));
        let mut istream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();

        let messages = StreamReader::new(&mut istream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 1);
        let msg = messages.first().unwrap();
        check_version_msg(msg);

        let messages = StreamReader::new(&mut istream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 1);
        let msg = messages.first().unwrap();
        assert_eq!(msg.magic, 0xd9b4bef9);
        assert_eq!(msg.payload, NetworkMessage::Verack, "Wrong message type, expected PingMessage");

        let messages = StreamReader::new(&mut istream, None).read_messages().unwrap();
        assert_eq!(messages.len(), 1);
        let msg = messages.first().unwrap();
        check_alert_msg(msg);

        handle.join().unwrap();
    }
}
