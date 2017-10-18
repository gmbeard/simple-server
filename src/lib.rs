//! A simple webserver.
//!
//! The `simple-server` crate is designed to give you the tools to to build
//! an HTTP server, based around the http crate, blocking I/O, and a
//! threadpool.
//!
//! We call it 'simple' want to keep the code small, and easy to
//! understand. This is why we're only using blocking I/O. Depending on
//! your needs, you may or may not want to choose another server.
//! However, just the simple stuff is often enough for many projects.
//!
//! # Examples
//!
//! At its core, `simple-server` contains a `Server`. The `Server` is
//! passed a handler upon creation, and the `listen` method is used
//! to start handling connections.
//!
//! The other types are from the `http` crate, and give you the ability
//! to work with various aspects of HTTP. The `Request`, `Response`, and
//! `ResponseBuilder` types are used by the handler you give to `Server`,
//! for example.
//!
//! To see examples of this crate in use, please consult the `examples`
//! directory.

#[macro_use]
extern crate log;

extern crate http;
extern crate httparse;
extern crate scoped_threadpool;

pub use http::Request;
pub use http::response::{Builder, Response, Parts};
pub use http::status::{InvalidStatusCode, StatusCode};
pub use http::method::Method;
use http::response::Builder as ResponseBuilder;

use scoped_threadpool::Pool;

use std::fs::File;
use std::io::prelude::*;
use std::net::{TcpListener, TcpStream};
use std::path::Path;

mod error;

pub use error::Error;

/// A web server.
///
/// This is the core type of this crate, and is used to create a new
/// server and listen for connections.
pub struct Server<'response> {
    handler: fn(Request<Vec<u8>>, ResponseBuilder) -> Result<Response<&'response [u8]>, Error>,
}


impl<'response> Server<'response> {
    /// Constructs a new server with the given handler.
    ///
    /// The handler function is called on all requests.
    ///
    /// # Errors
    ///
    /// The handler function returns a `Result` so that you may use `?` to
    /// handle errors. If a handler returns an `Err`, a 500 will be shown.
    ///
    /// If you'd like behavior other than that, return an `Ok(Response)` with
    /// the proper error code. In other words, this behavior is to gracefully
    /// handle errors you don't care about, not for properly handling
    /// non-`HTTP 200` responses.
    ///
    /// # Examples
    ///
    /// ```
    /// extern crate simple_server;
    ///
    /// use simple_server::Server;
    ///
    /// fn main() {
    ///     let server = Server::new(|request, mut response| {
    ///         Ok(response.body("Hello, world!".as_bytes())?)
    ///     });
    /// }
    /// ```
    pub fn new(
        handler: fn(Request<Vec<u8>>, ResponseBuilder) -> Result<Response<&'response [u8]>, Error>,
    ) -> Server {
        Server { handler }
    }

    /// Tells the server to listen on a specified host and port.
    ///
    /// A threadpool is created, and used to handle connections.
    /// The pool size is four threads.
    ///
    /// This method blocks forever.
    ///
    /// The `listen` method will also serve static files out of a `public`
    /// directory in the same directory as where it's run. If someone tries
    /// a path directory traversal attack, this will return a `404`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// extern crate simple_server;
    ///
    /// use simple_server::Server;
    ///
    /// fn main() {
    ///     let server = Server::new(|request, mut response| {
    ///         Ok(response.body("Hello, world!".as_bytes())?)
    ///     });
    ///
    ///     server.listen("127.0.0.1", "7979");
    /// }
    /// ```
    pub fn listen(&self, host: &str, port: &str) {
        let mut pool = Pool::new(4);
        let listener =
            TcpListener::bind(format!("{}:{}", host, port)).expect("Error starting the server.");

        info!("Server started at http://{}:{}", host, port);

        for stream in listener.incoming() {
            let stream = stream.expect("Error handling TCP stream.");

            pool.scoped(|scope| {
                scope.execute(|| {
                    self.handle_connection(stream).expect(
                        "Error handling connection.",
                    );
                });
            });
        }
    }

    fn read_request_object<'a>(&self, 
                               stream: &mut TcpStream, 
                               buf: &'a mut Vec<u8>) -> Result<Request<Vec<u8>>, Error> 
    {
        use std::io;

        let mut scratch = [0; 512];
        let bytes_read = {
            let bytes_read = stream.read(&mut scratch)?;
            if bytes_read == 0 {
                // Connection closed
                return Err(io::Error::new(io::ErrorKind::Other, "Connection closed").into());
            }
            bytes_read
        };

        buf.extend(&scratch[..bytes_read]);
        parse_request(&buf)
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<(), Error> {
        
        let mut buf = Vec::with_capacity(512);
        let request = {
            let req: Request<Vec<u8>>;
            loop {
                match self.read_request_object(&mut stream, &mut buf) {
                    Ok(r) => {
                        req = r;
                        break;
                    },
                    Err(Error::MoreBytesRequired) => {},
                    Err(e) => return Err(e),
                }
            }

            req
        };

        let mut response_builder = Response::builder();

        // first, we serve static files
        let fs_path = format!("public{}", request.uri());

        // ... you trying to do something bad?
        if fs_path.contains("./") || fs_path.contains("../") {
            // GET OUT
            response_builder.status(StatusCode::NOT_FOUND);

            let response = response_builder
                .body("<h1>404</h1><p>Not found!<p>".as_bytes())
                .unwrap();

            write_response(response, stream)?;
            return Ok(());
        }

        if Path::new(&fs_path).is_file() {
            let mut f = File::open(&fs_path)?;

            let mut source = Vec::new();

            f.read_to_end(&mut source)?;

            let response = response_builder.body(&*source)?;

            write_response(response, stream)?;
            return Ok(());
        }

        let response = (self.handler)(request, response_builder).unwrap_or_else(|_| {
            let mut response_builder = Response::builder();
            response_builder.status(StatusCode::INTERNAL_SERVER_ERROR);

            response_builder
                .body("<h1>500</h1><p>Internal Server Error!<p>".as_bytes())
                .unwrap()
        });

        Ok(write_response(response, stream)?)
    }
}

fn write_response(response: Response<&[u8]>, mut stream: TcpStream) -> Result<(), Error> {
    let text =
        format!(
        "HTTP/1.1 {} {}\r\n\r\n",
        response.status().as_str(),
        response.status().canonical_reason().unwrap(),
    );
    stream.write(text.as_bytes())?;

    stream.write(response.body())?;
    Ok(stream.flush()?)
}

fn parse_request(raw_request: &[u8]) -> Result<Request<Vec<u8>>, Error> {
    use httparse::Status;

    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = httparse::Request::new(&mut headers);

    let header_length = match req.parse(raw_request)? {
        Status::Complete(len) => {
            len as usize
        },
        Status::Partial => {
            return Err(Error::MoreBytesRequired);
        }
    };

//    let header_length = req.parse(raw_request)?.unwrap() as usize;

    let body = raw_request[header_length..].to_vec();
    let mut http_req = Request::builder();

    for header in req.headers {
        http_req.header(header.name, header.value);
    }

    let mut request = http_req.body(body)?;
    let path = req.path.unwrap();
    *request.uri_mut() = path.parse()?;

    Ok(request)
}
