# unrar

[![crates.io](https://img.shields.io/crates/v/unrar.svg)](https://crates.io/crates/unrar)
[![API docs](https://docs.rs/unrar/badge.svg)](https://docs.rs/unrar)
[![build](https://github.com/muja/unrar.rs/workflows/ci/badge.svg)](https://github.com/muja/unrar.rs/actions?query=workflow%3Aci)
[![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](./README.md)

High-level wrapper around the unrar C library provided by [rarlab](http://rarlab.com).

This library can only *extract* and *list* archives, it cannot *create* them.

Please look inside the [examples directory](./examples) to see how to use this library.
Specifically the [**lister**](./examples/lister.rs) example is well documented and advanced!

Basic example to list archive entries:

```rust,no_run
use unrar::Archive;

fn main() {
    for entry in Archive::new("archive.rar").open_for_listing().unwrap() {
        println!("{}", entry.unwrap());
    }
}
```

Run this example: `cargo run --example basic_list path/to/archive.rar`.
You can create an archive by using the `rar` CLI: `rar a archive.rar .`

# Overview

The primary type in this crate is [`Archive`]
which denotes an archive on the file system. `Archive` itself makes no
FS operations, unless one of the `open` methods are called, returning
an [`OpenArchive`].

# Archive

The [`Archive`] struct provides two major classes of methods:

   1. methods that do not touch the FS. These are opinionated utility methods
        that are based on RAR path conventions out in the wild. Most commonly, multipart
        files usually have extensions such as `.part08.rar` or `.r08.rar`. Since extracting
        must start at the first part, it may be helpful to figure that out using, for instance,
        [`archive.as_first_part()`](Archive::as_first_part)
   2. methods that open the underlying path in the specified mode
        (possible modes are [`List`], [`ListSplit`] and [`Process`]).
        These methods have the word `open` in them, are fallible operations,
        return [`OpenArchive`] inside a `Result` and are as follows:
        - [`open_for_listing`](Archive::open_for_listing) and
            [`open_for_listing_split`](Archive::open_for_listing_split): list the archive
            entries (skipping over content/payload)
        - [`open_for_processing`](Archive::open_for_processing): process archive entries
            as well as content/payload
        - [`break_open`](Archive::break_open): read archive even if an error is returned,
            if possible. The [`OpenMode`] must be provided
            explicitly.

# OpenArchive
An archive is opened in one of these three modes: [`List`], [`ListSplit`] or [`Process`].
This library does not provide random access into archives. Instead, files inside the archive
can only be processed as a stream, unidirectionally, front to back, alternating between
[`ReadHeader`] and [`ProcessFile`] operations (as dictated by the underlying C++ library).

That is the idea behind cursors:

## OpenArchive: Cursors

Via cursors, the archive keeps track what operation is permitted next:
   - [`CursorBeforeHeader`] -> [`ReadHeader`]
   - [`CursorBeforeFile`] -> [`ProcessFile`]

The library enforces this by making
use of the [typestate pattern](https://cliffle.com/blog/rust-typestate/). An archive, once
opened, starts in the `CursorBeforeHeader` state and, thus, must have its [`read_header`] method
called, which returns a new `OpenArchive` instance in the `CursorBeforeFile` state that only
exposes methods that internally map to the `ProcessFile` operation.
Which methods are accessible in each step depends on the archive's current state and the
mode it was opened in.

## Available methods for Open mode/Cursor position combinations
Here is an overview of what methods are exposed for the OpenMode/Cursor combinations:

| Open mode↓ ╲ Cursor position→| before header   | before file                                                            |
|------------------------------|-----------------|------------------------------------------------------------------------|
| [`List`], [`ListSplit`]      | [`read_header`] | [`skip`]                                                               |
| [`Process`]                  | [`read_header`] | [`skip`], [`read`], [`extract`], [`extract_to`], [`extract_with_base`] |

## OpenArchive: Iterator

Archives opened in [`List`] or [`ListSplit`] mode also implement [`Iterator`] whereas archives in
[`Process`] mode do not (though this may change in future releases). That is because the first
two will read and return headers while being forced to skip over the payload whereas the latter
has more sophisticated processing possibilities that's not easy to convey using an [`Iterator`].

# Example

For more sophisticated examples, please look inside the `examples/` folder.

Here's what a function that returns the first content of a file could look like:

```rust
fn first_file_content<P: AsRef<Path>>(path: P) -> UnrarResult<Vec<u8>> {
    let archive = Archive::new(&path).open_for_processing()?; // cursor: before header
    let archive = archive.read_header()?.expect("empty archive"); // cursor: before file
    dbg!(&archive.entry().filename);
    let (data, _rest) = archive.read()?; // cursor: before header
    Ok(data)
}
# use std::path::Path;
# use unrar::{Archive, UnrarResult};
#
# let data = first_file_content("data/version.rar").unwrap();
# assert_eq!(std::str::from_utf8(&data), Ok("unrar-0.4.0"));
```

[`read_header`]: OpenArchive::read_header
[`skip`]: OpenArchive::skip
[`read`]: OpenArchive::read
[`extract`]: OpenArchive::extract
[`extract_to`]: OpenArchive::extract_to
[`extract_with_base`]: OpenArchive::extract_with_base
[`ReadHeader`]: unrar_sys::RARReadHeaderEx
[`ProcessFile`]: unrar_sys::RARProcessFileW

# Features

- [x] Multipart files
- [x] Listing archives
- [x] Extracting them
- [x] Reading them into memory (without extracting)
- [x] Testing them
- [x] Encrypted archives with password
- [x] Linked statically against the unrar source.
- [x] Build unrar C++ code from source
- [x] Basic functionality that operates on filenames / paths (without reading archives)
- [x] Documentation / RustDoc
- [x] Test Suite
- [x] utilizes type system to enforce correct usage
- [ ] Well-designed errors (planned)
- [ ] TBD

# Non-Features
As this library is only a wrapper, these following features
are not easily feasible and as such not planned:

- Creating archives
- Random access into arbitrary archive entries
- Pure Rust implementation
- Processing archives from a file descriptor / fs::File handle
- Processing archives from a byte stream

# Contributing

Feel free to contribute! If you detect a bug, open an issue.

Pull requests are also welcome!

# Help

If you need help using the library, feel free to create a new discussion or open an issue.

# License

The parts authored by this library's contributors are licensed under either of

  * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
    http://www.apache.org/licenses/LICENSE-2.0)
  * MIT license ([LICENSE-MIT](LICENSE-MIT) or
    http://opensource.org/licenses/MIT)

at your option.

The embedded [C/C++ library](./unrar_sys/vendor/unrar) uses its own license. For more informations, see its [license file](./unrar_sys/vendor/unrar/license.txt).
