#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "alloc")]
use alloc::borrow::{Cow, ToOwned};

use core::fmt::Display;
use core::iter::FusedIterator;
use core::iter::{Enumerate, Peekable};
use core::ops::{Index, Range, RangeFrom};

#[cfg(feature = "uefi")]
use uefi::{Char16, Char8};

pub mod prelude {
    #[cfg(feature = "alloc")]
    pub use super::split as uefi_split;
    pub use super::Indexable as UefiSplitIndexable;
    pub use super::Split as UefiSplit;
}

pub trait Indexable:
    Index<Range<usize>, Output = Self::IndexOut> + Index<RangeFrom<usize>, Output = Self::IndexOut>
{
    type IndexOut: ?Sized;
    type Item: Eq;
    type AsIter<'a>: Iterator<Item = (usize, Self::Item)>
    where
        Self: 'a;

    fn as_iter(&self) -> Self::AsIter<'_>;

    const SPACE: Self::Item;
    const CARET: Self::Item;
    const QUOTE: Self::Item;
    const NUL: Self::Item;
}

impl Indexable for str {
    type IndexOut = str;
    type Item = char;
    type AsIter<'a> = core::str::CharIndices<'a>;
    fn as_iter(&self) -> Self::AsIter<'_> {
        self.char_indices()
    }

    const SPACE: Self::Item = ' ';
    const CARET: Self::Item = '^';
    const QUOTE: Self::Item = '"';
    const NUL: Self::Item = '\0';
}

macro_rules! impl_for_slice {
    ($Item:ty, $cvt:ident, $Back:ty) => {
        impl Indexable for [$Item] {
            type IndexOut = [$Item];
            type Item = $Item;
            type AsIter<'a> = Enumerate<core::iter::Copied<core::slice::Iter<'a, $Item>>> where $Item: 'a;
            fn as_iter(&self) -> Self::AsIter<'_> {
                self.iter().copied().enumerate()
            }

            const SPACE: Self::Item = $cvt!(b' ', $Back);
            const CARET: Self::Item = $cvt!(b'^', $Back);
            const QUOTE: Self::Item = $cvt!(b'"', $Back);
            const NUL: Self::Item = $cvt!(0, $Back);
        }
    };
    ($Item:ty, $cvt:ident) => {
        impl_for_slice!($Item, $cvt, $Item);
    };
}

macro_rules! cvt_primitive {
    ($val:expr, $Back:ty) => {
        $val as $Back
    };
}

#[cfg(feature = "uefi")]
macro_rules! cvt_transmute {
    ($val:expr, $Back:ty) => {
        unsafe { ::core::mem::transmute($val as $Back) }
    };
}

impl_for_slice!(u8, cvt_primitive);
impl_for_slice!(u16, cvt_primitive);
#[cfg(feature = "uefi")]
impl_for_slice!(Char8, cvt_transmute, u8);
#[cfg(feature = "uefi")]
impl_for_slice!(Char16, cvt_transmute, u16);

pub struct Split<'a, T: Indexable + ?Sized> {
    command_line: &'a T,
    iter: Peekable<T::AsIter<'a>>,
    fused: bool,
}

enum Ch<T> {
    Found { idx: usize, ch: T },
    NotFound(Option<usize>),
}

impl<'a, T: Indexable + ?Sized> Split<'a, T> {
    pub fn new(command_line: &'a T) -> Self {
        let iter = command_line.as_iter().peekable();
        Self {
            command_line,
            iter,
            fused: false,
        }
    }

    fn read_ch(&mut self) -> Option<(usize, T::Item)> {
        if self.fused {
            return None;
        }
        if let Some(res) = self.iter.next() {
            if res.1 == T::NUL {
                self.fused = true;
            }
            Some(res)
        } else {
            self.fused = true;
            None
        }
    }

    fn read_space(&mut self) {
        loop {
            let Some(item) = self.iter.peek() else {
                break;
            };
            if item.1 != T::SPACE {
                break;
            }
            self.read_ch().unwrap();
        }
    }

    fn find_next_ch(&mut self, pat: &[T::Item]) -> Ch<T::Item> {
        loop {
            let Some((idx, ch)) = self.read_ch() else {
                break;
            };
            if ch == T::CARET && self.read_ch().is_none() {
                break;
            }
            if pat.iter().any(|pat| *pat == ch) {
                return Ch::Found { idx, ch };
            }
        }
        let end_idx = self.iter.peek().map(|(idx, _)| *idx);
        Ch::NotFound(end_idx)
    }

    /// See <https://github.com/tianocore/edk2/blob/7f1a8cad9945674f068ff5e98a533280a7f0efb1/ShellPkg/Application/Shell/ShellParametersProtocol.c#L23-L57>
    fn find_end_of_arg(&mut self) -> Result<Option<usize>, ()> {
        loop {
            let ch = self.find_next_ch(&[T::SPACE, T::QUOTE, T::NUL]);
            // ends only if reaches whitespace or end
            let (idx, ch) = match ch {
                Ch::Found { idx, ch } => (idx, ch),
                Ch::NotFound(end) => return Ok(end),
            };
            if ch != T::QUOTE {
                return Ok(Some(idx));
            }
            if let Ch::Found { .. } = self.find_next_ch(&[T::QUOTE]) {
                continue;
            }
            return Err(());
        }
    }
}

impl<'a, T: Indexable + ?Sized> Iterator for Split<'a, T> {
    type Item = Arg<'a, T::IndexOut>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fused {
            return None;
        }
        self.read_space();
        let &(begin, _) = self.iter.peek()?;

        let end = match self.find_end_of_arg() {
            Err(()) => {
                self.fused = true;
                return None;
            }
            Ok(v) => v,
        };

        let raw_arg = if let Some(end) = end {
            if begin == end {
                self.fused = true;
                return None;
            }
            &self.command_line[begin..end]
        } else {
            &self.command_line[begin..]
        };

        Some(Arg { raw_arg })
    }
}
impl<T: Indexable + ?Sized> FusedIterator for Split<'_, T> {}

pub struct ArgIter<'a, T: 'a + Indexable + ?Sized> {
    raw_arg_iter: T::AsIter<'a>,
}

impl<T: Indexable + ?Sized> Iterator for ArgIter<'_, T> {
    type Item = T::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.raw_arg_iter.by_ref();
        loop {
            let (_, ch) = iter.next()?;
            if ch == T::QUOTE {
                continue;
            }
            if ch == T::CARET {
                return Some(iter.next()?.1);
            }
            return Some(ch);
        }
    }
}
impl<'a, T: Indexable + ?Sized> FusedIterator for ArgIter<'a, T> where T::AsIter<'a>: FusedIterator {}

#[derive(Debug, PartialEq, Eq)]
pub struct Arg<'a, T: ?Sized> {
    raw_arg: &'a T,
}
impl<T: ?Sized> Arg<'_, T> {
    pub fn raw(&self) -> &T {
        self.raw_arg
    }
}

impl<T: Indexable + ?Sized> Arg<'_, T> {
    pub fn iter(&self) -> ArgIter<'_, T> {
        ArgIter {
            raw_arg_iter: self.raw_arg.as_iter(),
        }
    }
}

impl<T: Indexable + ?Sized> Display for Arg<'_, T>
where
    T::Item: Display,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for ch in self.iter() {
            ch.fmt(f)?;
        }
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl<T: ToOwned + Indexable<IndexOut = T> + ?Sized> Arg<'_, T>
where
    T::Owned: FromIterator<T::Item>,
{
    #[inline]
    pub fn decode_to_owned(&self) -> T::Owned {
        self.iter().collect()
    }

    pub fn decode(&self) -> Cow<T> {
        let mut first = None;
        let mut last = None;
        let mut count = 0;
        let mut to_owned = false;
        for (_, ch) in self.raw_arg.as_iter() {
            count += 1;
            if last.is_some() {
                to_owned = true;
                break;
            }
            if ch == T::QUOTE {
                if first.is_none() {
                    first = Some(count);
                    if count != 1 {
                        to_owned = true;
                        break;
                    }
                } else {
                    last = Some(count)
                }
            }
            if ch == T::CARET {
                to_owned = true;
                break;
            }
        }
        if to_owned {
            return Cow::<T>::Owned(self.iter().collect());
        }
        if Some(1) == first && Some(count) == last {
            return Cow::Borrowed(&self.raw_arg[1..count - 1]);
        }
        Cow::Borrowed(self.raw_arg)
    }
}

#[cfg(feature = "alloc")]
pub fn split<T, B>(command_line: &T) -> B
where
    T: ToOwned + Indexable<IndexOut = T> + ?Sized,
    T::Owned: FromIterator<T::Item>,
    B: FromIterator<T::Owned>,
{
    Split::new(command_line)
        .map(|arg| arg.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg<T: Indexable + ?Sized>(raw_arg: &T) -> Arg<'_, T> {
        Arg { raw_arg }
    }

    #[test]
    fn split_str() {
        let mut s = Split::new("");
        assert_eq!(None, s.next());

        let mut s = Split::new(" \0invalid");
        assert_eq!(None, s.next());

        let mut s = Split::new(" pos -h --help \"quote\" quote\"in\"middle esc^\"ape \"unclosed");
        assert_eq!(Some(arg("pos")), s.next());
        assert_eq!(Some(arg("-h")), s.next());
        assert_eq!(Some(arg("--help")), s.next());
        assert_eq!(Some(arg("\"quote\"")), s.next());
        assert_eq!(Some(arg("quote\"in\"middle")), s.next());
        assert_eq!(Some(arg("esc^\"ape")), s.next());
        assert_eq!(None, s.next());
        assert_eq!(None, s.next());

        let mut s = Split::new("--single");
        assert_eq!(Some(arg("--single")), s.next());
        assert_eq!(None, s.next());

        let mut s = Split::new("program command -o --option argument\0invalid");
        assert_eq!(Some(arg("program")), s.next());
        assert_eq!(Some(arg("command")), s.next());
        assert_eq!(Some(arg("-o")), s.next());
        assert_eq!(Some(arg("--option")), s.next());
        assert_eq!(Some(arg("argument")), s.next());
        assert_eq!(None, s.next());
    }

    #[test]
    fn split_unicode() {
        let mut s = Split::new("早上好 hi 中国 现在我有冰淇淋");
        assert_eq!(Some(arg("早上好")), s.next());
        assert_eq!(Some(arg("hi")), s.next());
        assert_eq!(Some(arg("中国")), s.next());
        assert_eq!(Some(arg("现在我有冰淇淋")), s.next());
        assert_eq!(None, s.next());
    }

    #[test]
    fn arg_iter() {
        let a = arg("a^\"b^^c^d");
        let mut it = a.iter();
        assert_eq!(Some('a'), it.next());
        assert_eq!(Some('\"'), it.next());
        assert_eq!(Some('b'), it.next());
        assert_eq!(Some('^'), it.next());
        assert_eq!(Some('c'), it.next());
        assert_eq!(Some('d'), it.next());
        assert_eq!(None, it.next());

        let a = arg("l\"q\"r");
        let mut it = a.iter();
        assert_eq!(Some('l'), it.next());
        assert_eq!(Some('q'), it.next());
        assert_eq!(Some('r'), it.next());
        assert_eq!(None, it.next());
    }

    #[test]
    fn arg_format() {
        assert_eq!("abc", format!("{}", arg("abc")));
        assert_eq!("a^bc", format!("{}", arg("\"a^^bc\"")));
        assert_eq!("a\"bc", format!("{}", arg("a^\"bc")));
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn arg_decode() {
        assert_eq!(
            Cow::<str>::Owned(String::from("abc")),
            arg("a^b^c").decode()
        );
        assert_eq!(
            Cow::<str>::Owned(String::from("abc")),
            arg("a\"b\"c").decode()
        );
        assert_eq!(
            Cow::<str>::Owned(String::from("ab\"c")),
            arg("a^b^\"c").decode()
        );
        assert_eq!(Cow::<str>::Borrowed("abc"), arg("\"abc\"").decode());
    }

    #[test]
    fn slice_with_nul_split() {
        let cstr = b"argument --option\0invalid";
        let mut it = Split::new(cstr.as_slice());
        assert_eq!(Some(arg(b"argument".as_slice())), it.next());
        assert_eq!(Some(arg(b"--option".as_slice())), it.next());
        assert_eq!(None, it.next());
        assert_eq!(None, it.next());
    }

    #[cfg(feature = "uefi")]
    #[test]
    fn uefi_split() {
        use uefi::{cstr16, cstr8};
        let cstr = cstr16!("argument option");
        let mut it = Split::new(cstr.as_slice_with_nul());
        assert_eq!(Some(arg(cstr16!("argument").as_slice())), it.next());
        assert_eq!(Some(arg(cstr16!("option").as_slice())), it.next());
        assert_eq!(None, it.next());
        assert_eq!(None, it.next());

        let mut it = Split::new(cstr.to_u16_slice_with_nul());
        assert_eq!(Some(arg(cstr16!("argument").to_u16_slice())), it.next());
        assert_eq!(Some(arg(cstr16!("option").to_u16_slice())), it.next());
        assert_eq!(None, it.next());
        assert_eq!(None, it.next());

        let cstr = cstr16!("english 中文");
        let mut it = Split::new(cstr.as_slice_with_nul());
        assert_eq!(Some(arg(cstr16!("english").as_slice())), it.next());
        assert_eq!(Some(arg(cstr16!("中文").as_slice())), it.next());
        assert_eq!(None, it.next());
        assert_eq!(None, it.next());

        let cstr = cstr8!("argument option");
        let mut it = Split::new(cstr.as_bytes());
        assert_eq!(Some(arg(b"argument".as_slice())), it.next());
        assert_eq!(Some(arg(b"option".as_slice())), it.next());
        assert_eq!(None, it.next());
        assert_eq!(None, it.next());
    }
}
