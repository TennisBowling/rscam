#![feature(macro_rules)]
#![feature(slicing_syntax)]
#![feature(unsafe_destructor)]

extern crate libc;

use std::{io, fmt, str, error, default};

mod v4l2;


#[derive(Show)]
pub enum Error {
    /// I/O error when using the camera.
    Io(io::IoError),
    /// Unsupported frame interval.
    BadInterval,
    /// Unsupported resolution (width and/or height).
    BadResolution,
    /// Unsupported format of pixel.
    BadFormat,
    /// Unsupported field.
    BadField
}

impl error::FromError<io::IoError> for Error {
    fn from_error(err: io::IoError) -> Error {
        Error::Io(err)
    }
}

/// [Details](http://linuxtv.org/downloads/v4l-dvb-apis/field-order.html#v4l2-field).
#[repr(C)]
#[derive(Copy)]
pub enum Field {
    None = 1,
    Top,
    Bottom,
    Interplaced,
    SeqTB,
    SeqBT,
    Alternate,
    InterplacedTB,
    InterplacedBT
}

#[derive(Copy)]
pub struct Config<'a> {
    /**
     * The mix of numerator and denominator. v4l2 uses frame intervals instead of frame rates.
     * Default is `(1, 10)`.
     */
    pub interval: (u32, u32),
    /**
     * Width and height of frame.
     * Default is `(640, 480)`.
     */
    pub resolution: (u32, u32),
    /**
     * FourCC of format (e.g. `b"RGB3"`). Note that case matters.
     * Default is `b"YUYV"`.
     */
    pub format: &'a [u8],
    /**
     * Storage method of interlaced video.
     * Default is `Field::None` (progressive).
     */
    pub field: Field,
    /**
     * Number of buffers in the queue of camera.
     * Default is `2`.
     */
    pub nbuffers: u32
}

impl<'a> default::Default for Config<'a> {
    fn default() -> Config<'a> {
        Config {
            interval: (1, 10),
            resolution: (640, 480),
            format: b"YUYV",
            field: Field::None,
            nbuffers: 2
        }
    }
}

pub struct FormatInfo {
    /// FourCC of format (e.g. `b"H264"`).
    pub format: [u8; 4],
    /// Information about the format.
    pub description: String,
    /// Raw or compressed.
    pub compressed: bool,
    /// Whether it's transcoded from a different input format.
    pub emulated: bool,
    /// Resolutions and intervals for the format.
    pub modes: Vec<ModeInfo>
}

impl FormatInfo {
    fn new(fourcc: u32, desc: &[u8], flags: u32) -> FormatInfo {
        FormatInfo {
            format: [
                (fourcc >> 0 & 0xff) as u8,
                (fourcc >> 8 & 0xff) as u8,
                (fourcc >> 16 & 0xff) as u8,
                (fourcc >> 24 & 0xff) as u8
            ],

            description: unsafe {
                String::from_raw_buf(desc.as_ptr())
            },

            compressed: flags & v4l2::FMT_FLAG_COMPRESSED != 0,
            emulated: flags & v4l2::FMT_FLAG_EMULATED != 0,

            modes: vec![]
        }
    }

    fn fourcc(fmt: &[u8]) -> u32 {
        fmt[0] as u32 | (fmt[1] as u32) << 8 | (fmt[2] as u32) << 16 | (fmt[3] as u32) << 24
    }
}

impl fmt::Show for FormatInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({}{})", str::from_utf8(self.format.as_slice()).unwrap(),
            self.description, match (self.compressed, self.emulated) {
                (true, true) => ", compressed, emulated",
                (true, false) => ", compressed",
                (false, true) => ", emulated",
                _ => ""
            })
    }
}

pub struct ModeInfo {
    pub resolution: (u32, u32),
    pub intervals: Vec<(u32, u32)>
}

impl ModeInfo {
    pub fn new(resolution: (u32, u32)) -> ModeInfo {
        ModeInfo {
            resolution: resolution,
            intervals: vec![]
        }
    }
}

impl fmt::Show for ModeInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}x{}", self.resolution.0, self.resolution.1)
    }
}

pub struct Frame<'a> {
    /// Slice of one of the buffers.
    pub data: &'a [u8],
    /// Width and height of the frame.
    pub resolution: (u32, u32),
    /// FourCC of the format.
    pub format: [u8; 4],
    fd: int,
    buffer: v4l2::Buffer
}

#[unsafe_destructor]
impl<'a> Drop for Frame<'a> {
    #[allow(unused_must_use)]
    fn drop(&mut self) {
        v4l2::xioctl(self.fd, v4l2::VIDIOC_QBUF, &mut self.buffer);
    }
}

#[derive(Show, PartialEq)]
enum State {
    Idle,
    Streaming,
    Aborted
}

pub struct Camera<'a> {
    fd: int,
    state: State,
    resolution: (u32, u32),
    format: [u8; 4],
    buffers: Vec<&'a mut [u8]>
}

impl<'a> Camera<'a> {
    pub fn new(device: &str) -> io::IoResult<Camera> {
        Ok(Camera {
            fd: try!(v4l2::open(device)),
            state: State::Idle,
            resolution: (0, 0),
            format: [0; 4],
            buffers: vec![]
        })
    }

    /// Get detailed info about the available formats.
    pub fn formats(&self) -> io::IoResult<Vec<FormatInfo>> {
        let mut res = vec![];
        let mut fmt = v4l2::FmtDesc::new();
        let mut size = v4l2::Frmsizeenum::new();
        let mut ival = v4l2::Frmivalenum::new();

        // Get formats.
        while try!(v4l2::xioctl_valid(self.fd, v4l2::VIDIOC_ENUM_FMT, &mut fmt)) {
            let mut format = FormatInfo::new(fmt.pixelformat, &fmt.description, fmt.flags);

            size.index = 0;
            size.pixelformat = fmt.pixelformat;
            ival.pixelformat = fmt.pixelformat;

            // Get modes.
            while try!(v4l2::xioctl_valid(self.fd, v4l2::VIDIOC_ENUM_FRAMESIZES, &mut size)) {
                if size.ftype != v4l2::FRMSIZE_TYPE_DISCRETE {
                    size.index += 1;
                    continue;
                }

                let mut mode = ModeInfo::new((size.discrete.width, size.discrete.height));

                ival.index = 0;
                ival.width = mode.resolution.0;
                ival.height = mode.resolution.1;

                // Get intervals.
                while try!(v4l2::xioctl_valid(self.fd, v4l2::VIDIOC_ENUM_FRAMEINTERVALS,
                                              &mut ival)) {
                    if ival.ftype == v4l2::FRMIVAL_TYPE_DISCRET {
                        mode.intervals.push((ival.discrete.numerator, ival.discrete.denominator));
                    }

                    ival.index += 1;
                }

                format.modes.push(mode);
                size.index += 1;
            }

            res.push(format);
            fmt.index += 1;
        }

        Ok(res)
    }

    /**
     * Start streaming.
     *
     * # Panics
     * if recalled or called after `stop()`.
     */
    pub fn start(&mut self, config: &Config) -> Result<(), Error> {
        assert_eq!(self.state, State::Idle);

        try!(self.tune_format(config.resolution, config.format, config.field));
        try!(self.tune_stream(config.interval));
        try!(self.alloc_buffers(config.nbuffers));

        if let Err(err) = self.streamon() {
            let _ = self.free_buffers();
            return Err(Error::Io(err));
        }

        self.resolution = config.resolution;
        self.format = [config.format[0], config.format[1], config.format[2], config.format[3]];

        self.state = State::Streaming;

        Ok(())
    }

    /**
     * Blocking request of frame.
     * It dequeues buffer from a driver, which will be enqueueed after destructing `Frame`.
     *
     * # Panics
     * If called w/o streaming.
     */
    pub fn capture(&self) -> io::IoResult<Frame> {
        assert_eq!(self.state, State::Streaming);

        let mut buf = v4l2::Buffer::new();

        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_DQBUF, &mut buf));
        assert!(buf.index < self.buffers.len() as u32);

        Ok(Frame {
            data: self.buffers[buf.index as uint][0..buf.bytesused as uint],
            resolution: self.resolution,
            format: self.format,
            fd: self.fd,
            buffer: buf
        })
    }

    /**
     * Stop streaming.
     *
     * # Panics
     * If called w/o streaming.
     */
    pub fn stop(&mut self) -> io::IoResult<()> {
        assert_eq!(self.state, State::Streaming);

        try!(self.streamoff());
        try!(self.free_buffers());

        self.state = State::Aborted;

        Ok(())
    }

    fn tune_format(&self, resol: (u32, u32), format: &[u8], field: Field) -> Result<(), Error> {
        if format.len() != 4 {
            return Err(Error::BadFormat);
        }

        let fourcc = FormatInfo::fourcc(format);
        let mut fmt = v4l2::Format::new(resol, fourcc, field as u32);

        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_S_FMT, &mut fmt));

        if (fmt.fmt.width, fmt.fmt.height) != resol {
            return Err(Error::BadResolution);
        }

        if fourcc != fmt.fmt.pixelformat {
            return Err(Error::BadFormat);
        }

        if field as u32 != fmt.fmt.field {
            return Err(Error::BadField);
        }

        Ok(())
    }

    fn tune_stream(&self, interval: (u32, u32)) -> Result<(), Error> {
        let mut parm = v4l2::StreamParm::new(interval);

        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_S_PARM, &mut parm));
        let time = parm.parm.timeperframe;

        match (time.numerator * interval.1, time.denominator * interval.0) {
            (0, _) | (_, 0) => Err(Error::BadInterval),
            (x, y) if x != y => Err(Error::BadInterval),
            _ => Ok(())
        }
    }

    fn alloc_buffers(&mut self, nbuffers: u32) -> Result<(), Error> {
        let mut req = v4l2::RequestBuffers::new(nbuffers);

        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_REQBUFS, &mut req));

        for i in range(0, nbuffers) {
            let mut buf = v4l2::Buffer::new();
            buf.index = i;
            try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_QUERYBUF, &mut buf));

            let region = try!(v4l2::mmap(buf.length as uint, self.fd, buf.m));

            self.buffers.push(region);
        }

        Ok(())
    }

    fn free_buffers(&mut self) -> io::IoResult<()> {
        let mut res = Ok(());

        for buffer in self.buffers.iter_mut() {
            if let (&Ok(_), Err(err)) = (&res, v4l2::munmap(*buffer)) {
                res = Err(err);
            }
        }

        self.buffers.clear();
        res
    }

    fn streamon(&self) -> io::IoResult<()> {
        for i in range(0, self.buffers.len()) {
            let mut buf = v4l2::Buffer::new();
            buf.index = i as u32;

            try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_QBUF, &mut buf));
        }

        let mut typ = v4l2::BUF_TYPE_VIDEO_CAPTURE;
        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_STREAMON, &mut typ));

        Ok(())
    }

    fn streamoff(&mut self) -> io::IoResult<()> {
        let mut typ = v4l2::BUF_TYPE_VIDEO_CAPTURE;
        try!(v4l2::xioctl(self.fd, v4l2::VIDIOC_STREAMOFF, &mut typ));

        Ok(())
    }
}

#[unsafe_destructor]
impl<'a> Drop for Camera<'a> {
    #[allow(unused_must_use)]
    fn drop(&mut self) {
        if self.state == State::Streaming {
            self.stop();
        }

        v4l2::close(self.fd);
    }
}

/// Alias for `Camera::new()`.
pub fn new(device: &str) -> io::IoResult<Camera> {
    Camera::new(device)
}
