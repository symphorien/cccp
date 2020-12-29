# `cccp`: Carefully Checked Copy

`cccp` is a small tool which is designed to copy a file, a tree of files or a
disk image to an untrustworthy USB drive. It will copy the files and reread them
to check that the copy was correct. If extra files are on the target, they
will be removed. Metadata and permissions are not copied.


### Examples

Copy a file to a USB drive
```
cccp myfile.tar.gz /run/media/username/usbdrive/myfile.tar.gz
```

Copy a directory recursively:
```
cccp thedirectory /run/media/username/usbdrive/thedirectory
```

Copy an iso image to a USB drive at `/dev/sdx` to make a live USB:
```
cccp distro.iso /dev/sdx
```

**Warning**: if `file` is a file and `dir` a directory,
```
cccp file dir
```
does not copy `file` inside `dir` but removes `dir` along with all its content
and replaces it by a file named `dir` with the same content as `file`.
As a general rule, `cccp` strives to make the destination path identical to the
source path.

### Caches

Just rereading files after the copy is not enough. Notably, the kernel may keep
the files we just copied in the page cache, and when we attempt to reread from the
USB drive, we are only served this RAM cache. This would hide any defects in the
copy.

There are at least two levels of caching: the Linux kernel has a page cache, and
high-end USB drives may have their own cache.

`cccp` uses various methods to avoid these caches which have different levels of
efficiency and privilege requirements.

* `--mode=directio` opens files with `O_DIRECT` which tells the kernel to
bypass the page cache. Some filesystem do not support this method, and
copy throughput might suffer.
* `--mode=vm` drops the full page cache after the copy. This requires root privilege,
and will affect the performance of the full system.
* `--mode=umount` bypasses the page cache by unmounting and remounting the target
filesystem with udisks. For USB drives, this usually requires no privileges, but
you must not be using the drive in any other way.

None of these methods is able to bypass a possible cache in the drive. There
are plans for that, but in the mean time, you can use the manual method: run `cccp`
with whatever method you want, remove the usb drive, plug it in again, and rerun
`cccp`. If `cccp` does not display a message about fixing any file, then the
first copy was successful.
