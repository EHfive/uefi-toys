# UEFI Toys

## reset

A UEFI application to set [OSIndications](https://uefi.org/specs/UEFI/2.10/08_Services_Runtime_Services.html#exchanging-information-between-the-os-and-firmware) flags and reset system.

You can also set default command-line options with environment variable `UEFI_RESET_DEFAULT_CMD` at compile-time.

For example, you can chainload the following efi in an UEFI boot loader to reboot system to firmware UI.

```
export UEFI_RESET_DEFAULT_CMD="uefi-reset.efi firmware"
cargo build --package uefi-reset
```

## loopdrv

A UEFI loopback service driver similar to loop driver on Linux.
It additionally provides device-mapper like linear concatting interface for file patching.

See [LoopControlProtocol](loopdrv/src/driver/loop_ctl.rs) and [LoopProtocol](loopdrv/src/driver/loopback/loop_pt.rs) for protocols.

## lopatch

A UEFI application to attach image file to loopback device with loopdrv similar to `losetup` on Linux.
It also supports file patching for ISO96660 image,
this can be used to append a custom initramfs hence hijacking the init process.

### Build

```
cargo build --target x86_64-unknown-uefi
stat target/x86_64-unknown-uefi/debug/*.efi
```

You can also build for target "aarch64-unknown-uefi" or "i686-unknown-uefi" that powered by Rust/LLVM's cross-compile capability.

### Usage

You need to operate under UEFI shell.

#### (Optional) Load filesystem drivers

Load filesystem drivers from [efifs](https://github.com/pbatard/efifs) if your files are not resides in FAT partition.

Drivers need to be copies to the ESP partition or anywhere UEFI shell can read.

```
Shell> FS0:
FS0:\> load ext2_x64.efi
FS0:\> load btrfs_x64.efi
FS0:\> load ntfs_x64.efi
FS0:\> map -r
```

#### Load the loopdrv

```
FS0:\> load uefi-loopdrv.efi
```

#### Attach image file with lopatch

<details>
  <summary>Take a look at help message</summary>

```
FS0:\> uefi-lopatch --help
Usage: FS0:\uefi-lopatch.efi [OPTIONS] IMAGE_FILE

  Setup a loopback device for IMAGE_FILE with optional ISO file
  patching for IMAGE_FILE contains an iso9660 filesystem

  -h, --help            Print this help and exit
  -i, --id NUM          Loopback ID to use, find a free one if omitted
  -r, --read-only       Mark read-only
  -P                    Mark that IMAGE_FILE has disk partitioning
  -l, --list            List all loopback devices
  -d, --detach          Detach the loopback device specified by -i/--id

ISO Patching Options:
  -s, --search PATH     Search file in ISO to patch, each --search/--pattern
                        should followed with one or more action options, i.e.
                        --append, --meta-cpio or --replace. A file matches if
                        PATH is a valid file path relative to any parent
                        directory. The action would applies to all files found.
  -p, --pattern REGEX   Use regular expression instead to match file path
  -a, --append FILE     Append FILE data to end of the matched ISO file
  -m, --meta-cpio       Append mapping metadata file as CPIO
  -R, --replace FILE    Replace data of the matched ISO file with FILE data

EXAMPLE:
  * Append a cpio to initramfs file in Live CD ISO and setup loopback
  FS0:\uefi-lopatch.efi -s initramfs-linux.img -a patch-init.cpio archlinux.iso

  * Attach a FAT image to a free loopback device
  FS0:\uefi-lopatch.efi fat.img
```

</details>

Attach an ISO image in FS1 to loopback device with unit number 0

```
FS1:\> FS0:\uefi-lopatch.efi --read-only --id 0 archlinux.iso
FS1:\> map -r
```

let say FS2 is partition on the loopback device that just been attached.

```
FS1:\> FS2:
FS2:\> ls
```

Detach the loopback and re-attach with patching, lopatch would search a file named "initramfs-linux.img" and append data of "patch-init.cpio" to the end of former.
This is achieved by modifying the ISO9660 directory record of the file to re-point to re-positioned patched file.
Suppose patch-init.cpio is a cpio containing a hijacking `/init`,
which also setup ISO image as loopback device in boot stage 1 hence the Live CD booting can continues to stage 2.

```
FS2:\> FS0:\uefi-lopatch.efi --id 0 --detach
FS1:\> FS0:\uefi-lopatch.efi -r -s initramfs-linux.img -a FS0:\patch-init.cpio -m archlinux.iso
FS1:\> map -r
```

Boot the ISO boot loader

```
FS1:\> FS2:\efi\boot\bootx64.efi
```
