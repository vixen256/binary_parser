use std::{
	collections::VecDeque,
	io::{self, *},
	path::Path,
};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, BinaryParserError>;

#[derive(Error, Debug)]
pub enum BinaryParserError {
	#[error("IO error")]
	Io(#[from] io::Error),
	#[error("UTF8 parse error")]
	Utf8(#[from] std::str::Utf8Error),
}

#[derive(Default)]
pub struct BinaryParser<'a> {
	inner: Cursor<Vec<u8>>,
	bases: Vec<u64>,
	scheduled_writes: VecDeque<ScheduledWrite<'a>>,
	big_endian: bool,
}

struct ScheduledWrite<'a> {
	func: Box<dyn FnOnce(&mut BinaryParser<'a>) -> Result<()> + 'a>,
	position: u64,
}

macro_rules! int_impl {
	($ty: ty, $bytes: literal) => {
		paste::item! {
			pub fn [< read_ $ty >] (&mut self) -> Result<$ty> {
				let mut buf: [u8; $bytes] = [0; $bytes];
				self.inner.read_exact(&mut buf)?;
				let val = if self.big_endian {
					$ty::from_be_bytes(buf)
				} else {
					$ty::from_le_bytes(buf)
				};
				Ok(val)
			}

			pub fn [< read_ $ty _array >] (&mut self, count: u64) -> Result<Vec<$ty>> {
				let mut data = vec![];
				for _ in 0..count {
					let mut buf: [u8; $bytes] = [0; $bytes];
					self.inner.read_exact(&mut buf)?;
					let val = if self.big_endian {
						$ty::from_be_bytes(buf)
					} else {
						$ty::from_le_bytes(buf)
					};
					data.push(val);
				}
				Ok(data)
			}

			pub fn [< write_ $ty >] (&mut self, data: $ty) -> Result<()> {
				let buf = if self.big_endian {
					$ty::to_be_bytes(data)
				} else {
					$ty::to_le_bytes(data)
				};
				self.inner.write_all(&buf)?;
				Ok(())
			}

			pub fn [< write_ $ty _array >] (&mut self, data: &[$ty]) -> Result<()> {
				for elem in data {
					let buf = if self.big_endian {
						$ty::to_be_bytes(*elem)
					} else {
						$ty::to_le_bytes(*elem)
					};
					self.inner.write_all(&buf)?;
				}
				Ok(())
			}
		}
	};
}

impl<'a> BinaryParser<'a> {
	pub fn new() -> Self {
		Self {
			inner: Cursor::new(vec![]),
			bases: vec![],
			scheduled_writes: VecDeque::new(),
			big_endian: false,
		}
	}

	pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
		let buf = std::fs::read(path)?;
		Ok(Self {
			inner: Cursor::new(buf),
			bases: vec![],
			scheduled_writes: VecDeque::new(),
			big_endian: false,
		})
	}

	pub fn from_buf<B: Into<Vec<u8>>>(buf: B) -> Self {
		Self {
			inner: Cursor::new(buf.into()),
			bases: vec![],
			scheduled_writes: VecDeque::new(),
			big_endian: false,
		}
	}

	pub fn set_big_endian(&mut self, be: bool) {
		self.big_endian = be;
	}

	pub fn to_file<P: AsRef<Path>>(self, path: P) -> Result<()> {
		let parser = self.finish_writes()?;
		let mut file = std::fs::File::create(path)?;
		file.write_all(parser.inner.get_ref())?;
		Ok(())
	}

	pub fn to_buf(self) -> Result<Vec<u8>> {
		let parser = self.finish_writes()?;
		Ok(parser.inner.into_inner())
	}

	// Returns None if writes are still pending
	pub fn to_buf_const(&self) -> Option<&Vec<u8>> {
		if self.pending_writes() {
			None
		} else {
			Some(self.inner.get_ref())
		}
	}

	pub fn position(&self) -> u64 {
		self.inner.position()
	}

	int_impl!(u8, 1);
	int_impl!(u16, 2);
	int_impl!(u32, 4);
	int_impl!(u64, 8);
	int_impl!(i8, 1);
	int_impl!(i16, 2);
	int_impl!(i32, 4);
	int_impl!(i64, 8);
	int_impl!(f32, 4);
	int_impl!(f64, 8);

	pub fn read_null_string(&mut self) -> Result<String> {
		let mut buf = vec![];
		self.inner.read_until(0x00, &mut buf)?;
		buf.pop();
		Ok(String::from(std::str::from_utf8(&buf)?))
	}

	pub fn read_string(&mut self, length: usize) -> Result<String> {
		let mut buf = vec![0; length];
		self.inner.read_exact(&mut buf)?;
		Ok(String::from(std::str::from_utf8(&buf)?))
	}

	pub fn read_null_string_pointer(&mut self) -> Result<String> {
		self.read_pointer(|reader| reader.read_null_string())
	}

	pub fn write_string(&mut self, data: &str) -> Result<()> {
		let buf = data.as_bytes();
		self.inner.write_all(buf)?;
		Ok(())
	}

	pub fn write_null_string(&mut self, data: &str) -> Result<()> {
		let buf = data.as_bytes();
		self.inner.write_all(buf)?;
		self.write_u8(0)?;
		Ok(())
	}

	pub fn write_null_string_pointer(&mut self, data: &str) -> Result<()> {
		let buf = data.as_bytes().to_vec();
		self.write_pointer(move |writer| {
			writer.inner.write_all(&buf)?;
			writer.write_u8(0)?;
			Ok(())
		})?;
		Ok(())
	}

	pub fn read_buf(&mut self, length: usize) -> Result<Vec<u8>> {
		let mut buf = vec![0; length];
		self.inner.read_exact(&mut buf)?;
		Ok(buf)
	}

	pub fn write_buf(&mut self, data: &[u8]) -> Result<()> {
		self.inner.write_all(data)?;
		Ok(())
	}

	pub fn read_parser(&mut self, length: usize) -> Result<Self> {
		let buf = self.read_buf(length)?;
		Ok(Self::from_buf(buf))
	}

	pub fn write_parser(&mut self, parser: Self) -> Result<()> {
		let new = parser.to_buf()?;
		self.write_buf(&new)
	}

	pub fn seek(&mut self, pos: SeekFrom) -> Result<()> {
		self.inner.seek(pos)?;
		Ok(())
	}

	pub fn push_base(&mut self) {
		self.bases.push(self.position());
	}

	pub fn pop_base(&mut self) {
		self.bases.pop();
	}

	pub fn read_pointer<T, F>(&mut self, func: F) -> Result<T>
	where
		F: FnOnce(&mut Self) -> Result<T>,
	{
		let pos = self.position() + 4;
		let offset = self.read_u32()? as u64;
		let offset = offset + self.bases.last().unwrap_or(&0);
		self.seek(SeekFrom::Start(offset))?;
		let res = func(self);
		self.seek(SeekFrom::Start(pos))?;
		res
	}

	pub fn write_pointer<F>(&mut self, func: F) -> Result<()>
	where
		F: FnOnce(&mut Self) -> Result<()> + 'a,
	{
		let position = self.position();
		self.scheduled_writes.push_back(ScheduledWrite {
			func: Box::new(func),
			position,
		});
		self.seek(SeekFrom::Current(4))?;

		Ok(())
	}

	pub fn pending_writes(&self) -> bool {
		!self.scheduled_writes.is_empty()
	}

	pub fn finish_writes(self) -> Result<Self> {
		let mut new = Self {
			bases: self.bases,
			big_endian: self.big_endian,
			inner: self.inner,
			scheduled_writes: VecDeque::new(),
		};
		for write in self.scheduled_writes {
			let pos = new.position();
			(write.func)(&mut new)?;
			let new_pos = new.position();
			new.seek(SeekFrom::Start(write.position))?;
			new.write_u32(pos as u32)?;
			new.seek(SeekFrom::Start(new_pos))?;
		}
		Ok(new)
	}

	pub fn align_seek(&mut self, alignment: u64) -> Result<()> {
		let pos = self.position();
		let offset = if pos & (alignment - 1) != 0 {
			(pos & !(alignment - 1)) + alignment
		} else {
			pos
		};
		self.seek(SeekFrom::Start(offset))
	}

	pub fn align_write(&mut self, alignment: u64) -> Result<()> {
		while self.position() & (alignment - 1) != 0 {
			self.write_u8(0)?;
		}
		Ok(())
	}
}
