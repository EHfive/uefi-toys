#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "alloc")]
use alloc::borrow::{Cow, ToOwned};

use core::fmt::Display;
use core::iter::FusedIterator;
use core::iter::{Enumerate, Peekable};
use core::ops::Range;

#[cfg(feature = "uefi")]
use uefi::Char16;

pub mod prelude {
    #[cfg(feature = "alloc")]
    pub use super::split as uefi_split;
    pub use super::Indexable as UefiSplitIndexable;
    pub use super::Split as UefiSplit;
}

pub trait Indexable {
    type Output: ?Sized;
    type Item: Eq;
    type AsIter<'a>: Iterator<Item = Self::Item>
    where
        Self: 'a;

    fn as_iter(&self) -> Self::AsIter<'_>;
    fn index_range(&self, range: Range<usize>) -> &Self::Output;

    const SPACE: Self::Item;
    const CARET: Self::Item;
    const QUOTE: Self::Item;
}

#[allow(unconditional_recursion, clippy::only_used_in_recursion)]
impl<'a, T: Indexable> Indexable for &'a T {
    type Output = T::Output;
    type Item = T::Item;
    type AsIter<'b> = T::AsIter<'a> where Self: 'b;

    fn as_iter(&self) -> Self::AsIter<'_> {
        self.as_iter()
    }
    fn index_range(&self, range: Range<usize>) -> &Self::Output {
        self.index_range(range)
    }

    const SPACE: Self::Item = T::SPACE;
    const CARET: Self::Item = T::CARET;
    const QUOTE: Self::Item = T::QUOTE;
}

impl Indexable for str {
    type Output = str;
    type Item = char;
    type AsIter<'a> = core::str::Chars<'a>;
    fn as_iter(&self) -> Self::AsIter<'_> {
        self.chars()
    }
    fn index_range(&self, range: Range<usize>) -> &Self::Output {
        &self[range]
    }

    const SPACE: Self::Item = ' ';
    const CARET: Self::Item = '^';
    const QUOTE: Self::Item = '"';
}

#[cfg(feature = "uefi")]
impl Indexable for [Char16] {
    type Output = [Char16];
    type Item = Char16;
    type AsIter<'a> = core::iter::Copied<core::slice::Iter<'a, Char16>>;
    fn as_iter(&self) -> Self::AsIter<'_> {
        self.iter().copied()
    }
    fn index_range(&self, range: Range<usize>) -> &Self::Output {
        &self[range]
    }

    const SPACE: Self::Item = unsafe { Char16::from_u16_unchecked('^' as _) };
    const CARET: Self::Item = unsafe { Char16::from_u16_unchecked('^' as _) };
    const QUOTE: Self::Item = unsafe { Char16::from_u16_unchecked('"' as _) };
}

pub struct Split<'a, T: Indexable + ?Sized> {
    command_line: &'a T,
    iter: Peekable<Enumerate<T::AsIter<'a>>>,
}

impl<'a, T: Indexable + ?Sized> Split<'a, T> {
    pub fn new(command_line: &'a T) -> Self {
        let iter = command_line.as_iter().enumerate().peekable();
        Self { command_line, iter }
    }

    fn read_space(&mut self) {
        loop {
            let Some(item) = self.iter.peek() else {
                break;
            };
            if item.1 != T::SPACE {
                break;
            }
            self.iter.next().unwrap();
        }
    }

    fn find_next_ch(&mut self, pat: &[T::Item]) -> (usize, Option<T::Item>) {
        let mut end_idx = 0;
        loop {
            let Some((idx, ch)) = self.iter.next() else {
                break;
            };
            end_idx = idx + 1;
            if ch == T::CARET && self.iter.next().is_none() {
                break;
            }
            if pat.iter().any(|pat| *pat == ch) {
                return (idx, Some(ch));
            }
        }
        (end_idx, None)
    }

    /// See <https://github.com/tianocore/edk2/blob/7f1a8cad9945674f068ff5e98a533280a7f0efb1/ShellPkg/Application/Shell/ShellParametersProtocol.c#L23-L57>
    fn find_end_of_arg(&mut self) -> Result<usize, ()> {
        loop {
            let (first, ch) = self.find_next_ch(&[T::SPACE, T::QUOTE]);
            // ends only if reaches whitespace or end
            let Some(ch) = ch else {
                return Ok(first);
            };
            if ch == T::SPACE {
                return Ok(first);
            }
            let (_, ch) = self.find_next_ch(&[T::QUOTE]);
            if ch.is_none() {
                // unclosed quote
                return Err(());
            }
        }
    }
}

impl<'a, T: Indexable + ?Sized> Iterator for Split<'a, T> {
    type Item = Arg<'a, T::Output>;

    fn next(&mut self) -> Option<Self::Item> {
        self.read_space();
        let &(begin, _) = self.iter.peek()?;

        let end = match self.find_end_of_arg() {
            Err(()) => return None,
            Ok(v) => v,
        };

        if begin == end {
            return None;
        }

        Some(Arg {
            raw_arg: self.command_line.index_range(begin..end),
        })
    }
}
impl<'a, T: Indexable + ?Sized> FusedIterator for Split<'a, T> where T::AsIter<'a>: FusedIterator {}

pub struct ArgIter<'a, T: 'a + Indexable + ?Sized> {
    raw_arg_iter: T::AsIter<'a>,
}

impl<T: Indexable + ?Sized> Iterator for ArgIter<'_, T> {
    type Item = T::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let iter = self.raw_arg_iter.by_ref();
        loop {
            let ch = iter.next()?;
            if ch == T::QUOTE {
                continue;
            }
            if ch == T::CARET {
                return iter.next();
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
impl<T: ToOwned + Indexable<Output = T> + ?Sized> Arg<'_, T>
where
    T::Owned: FromIterator<T::Item>,
{
    pub fn decode_to_owned(&self) -> T::Owned {
        self.iter().collect()
    }

    pub fn decode(&self) -> Cow<T> {
        let mut first = None;
        let mut last = None;
        let mut count = 0;
        let mut to_owned = false;
        for ch in self.raw_arg.as_iter() {
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
            return Cow::Borrowed(self.raw_arg.index_range(1..count - 1));
        }
        Cow::Borrowed(self.raw_arg)
    }
}

#[cfg(feature = "alloc")]
pub fn split<T, B>(command_line: &T) -> B
where
    T: ToOwned + Indexable<Output = T> + ?Sized,
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

    fn arg(raw_arg: &str) -> Arg<'_, str> {
        Arg { raw_arg }
    }

    #[test]
    fn split_str() {
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

        let mut s = Split::new("program command -o --option argument");
        assert_eq!(Some(arg("program")), s.next());
        assert_eq!(Some(arg("command")), s.next());
        assert_eq!(Some(arg("-o")), s.next());
        assert_eq!(Some(arg("--option")), s.next());
        assert_eq!(Some(arg("argument")), s.next());
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
}
