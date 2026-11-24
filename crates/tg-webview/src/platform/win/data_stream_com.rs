//! COM `IStream` adapter wrapping a Rust [`DataStream`].
//!
//! Port of `Telegram/lib_webview/webview/platform/win/webview_windows_data_stream.h` and
//! `Telegram/lib_webview/webview/platform/win/webview_windows_data_stream.cpp`.

use std::ffi::c_void;
use std::io::SeekFrom;
use std::sync::Mutex;

use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATSTG, STGC, STGTY_STREAM,
    STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows_core::{HRESULT, Ref, Result as WinResult, implement};

use crate::data_stream::DataStream;

/// COM `IStream` implementation backed by any Rust [`DataStream`].
///
/// Mirrors `Webview::DataStreamCOM` (`webview_windows_data_stream.cpp`).
#[implement(IStream)]
pub(crate) struct DataStreamCom {
    wrapped: Mutex<Box<dyn DataStream>>,
}

impl DataStreamCom {
    /// Wraps `stream` in a COM-reference-counted `IStream`.
    pub(crate) fn into_istream(stream: Box<dyn DataStream>) -> IStream {
        Self {
            wrapped: Mutex::new(stream),
        }
        .into()
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl ISequentialStream_Impl for DataStreamCom_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() && cb > 0 {
            return E_POINTER;
        }
        let Ok(mut stream) = self.wrapped.lock() else {
            return E_FAIL;
        };
        let buf = if cb == 0 {
            &mut []
        } else {
            // SAFETY: COM caller guarantees `pv` points to a writable buffer of at least `cb` bytes.
            unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) }
        };
        match stream.read(buf) {
            Ok(read_bytes) => {
                let read_u32 = u32::try_from(read_bytes).unwrap_or(u32::MAX);
                if !pcbread.is_null() {
                    // SAFETY: COM caller provided a valid writable output pointer for `pcbRead`.
                    unsafe {
                        *pcbread = read_u32;
                    }
                }
                if read_u32 == cb { S_OK } else { S_FALSE }
            }
            Err(_) => E_FAIL,
        }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        E_NOTIMPL
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IStream_Impl for DataStreamCom_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> WinResult<()> {
        let pos = match dworigin {
            STREAM_SEEK_SET => {
                let offset =
                    u64::try_from(dlibmove).map_err(|_| windows_core::Error::from(E_FAIL))?;
                SeekFrom::Start(offset)
            }
            STREAM_SEEK_CUR => SeekFrom::Current(dlibmove),
            STREAM_SEEK_END => SeekFrom::End(dlibmove),
            _ => return Err(windows_core::Error::from(E_FAIL)),
        };
        let mut stream = self
            .wrapped
            .lock()
            .map_err(|_| windows_core::Error::from(E_FAIL))?;
        let new_pos = stream
            .seek(pos)
            .map_err(|_| windows_core::Error::from(E_FAIL))?;
        if !plibnewposition.is_null() {
            // SAFETY: COM caller provided a valid writable output pointer for `plibNewPosition`.
            unsafe {
                *plibnewposition = new_pos;
            }
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn CopyTo(
        &self,
        _pstm: Ref<IStream>,
        _cb: u64,
        _pcbread: *mut u64,
        _pcbwritten: *mut u64,
    ) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn Revert(&self) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> WinResult<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> WinResult<()> {
        if pstatstg.is_null() {
            return Err(windows_core::Error::from(E_POINTER));
        }
        let stream = self
            .wrapped
            .lock()
            .map_err(|_| windows_core::Error::from(E_FAIL))?;
        let size = stream.size();
        if size < 0 {
            return Err(windows_core::Error::from(E_NOTIMPL));
        }
        // SAFETY: COM caller provided a valid writable `STATSTG` pointer.
        unsafe {
            *pstatstg = STATSTG {
                r#type: STGTY_STREAM.0 as u32,
                cbSize: size as u64,
                ..STATSTG::default()
            };
        }
        Ok(())
    }

    fn Clone(&self) -> WinResult<IStream> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_stream::MemoryDataStream;
    use windows::Win32::System::Com::STATFLAG_NONAME;

    #[test]
    fn com_istream_read_seek_stat_and_write_notimpl() {
        let mem = MemoryDataStream::new(b"0123456789".to_vec(), "application/octet-stream");
        let istream = DataStreamCom::into_istream(Box::new(mem));

        // SAFETY: Calling standard COM `IStream` methods on our own `DataStreamCom` instance.
        unsafe {
            let mut stat = STATSTG::default();
            istream.Stat(&mut stat, STATFLAG_NONAME).unwrap();
            assert_eq!(stat.cbSize, 10);

            let mut buf = [0u8; 4];
            let mut read = 0u32;
            let hr = istream.Read(buf.as_mut_ptr().cast(), 4, Some(&mut read));
            assert_eq!(hr, S_OK);
            assert_eq!(read, 4);
            assert_eq!(&buf, b"0123");

            let mut new_pos = 0u64;
            istream
                .Seek(2, STREAM_SEEK_CUR, Some(&mut new_pos))
                .unwrap();
            assert_eq!(new_pos, 6);

            let mut tail = [0u8; 8];
            let hr = istream.Read(tail.as_mut_ptr().cast(), 8, Some(&mut read));
            assert_eq!(hr, S_FALSE);
            assert_eq!(read, 4);
            assert_eq!(&tail[..4], b"6789");

            istream
                .Seek(-3, STREAM_SEEK_END, Some(&mut new_pos))
                .unwrap();
            assert_eq!(new_pos, 7);

            let mut written = 0u32;
            let write_hr = istream.Write(buf.as_ptr().cast(), 4, Some(&mut written));
            assert_eq!(write_hr, E_NOTIMPL);
        }
    }
}
