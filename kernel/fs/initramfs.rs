//! Initramfs parser.
//! https://www.kernel.org/doc/html/latest/driver-api/early-userspace/buffer-format.html
use crate::{
    fs::{
        file_system::FileSystem,
        inode::{DirEntry, Directory, FileLike, INode, INodeNo},
        path::Path,
        stat::FileMode,
        stat::{Stat, S_IFDIR},
    },
    result::{Errno, Error, Result},
};
use alloc::sync::Arc;
use core::str::from_utf8_unchecked;
use hashbrown::HashMap;
use penguin_utils::byte_size::ByteSize;
use penguin_utils::bytes_parser::BytesParser;
use penguin_utils::once::Once;

fn parse_str_field(bytes: &[u8]) -> &str {
    unsafe { from_utf8_unchecked(bytes) }
}

fn parse_hex_field(bytes: &[u8]) -> usize {
    usize::from_str_radix(parse_str_field(bytes), 16).unwrap()
}

pub static INITRAM_FS: Once<Arc<InitramFs>> = Once::new();

struct InitramFsFile {
    data: &'static [u8],
    stat: Stat,
}

impl FileLike for InitramFsFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
        buf.copy_from_slice(&self.data[offset..offset + buf.len()]);
        Ok(buf.len())
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize> {
        Err(Error::new(Errno::EROFS))
    }

    fn stat(&self) -> Result<Stat> {
        Ok(self.stat.clone())
    }
}

enum InitramFsINode {
    File(Arc<InitramFsFile>),
    Directory(Arc<InitramFsDir>),
}
struct InitramFsDir {
    stat: Stat,
    files: HashMap<&'static str, InitramFsINode>,
}

impl Directory for InitramFsDir {
    fn stat(&self) -> Result<Stat> {
        Ok(self.stat.clone())
    }

    fn lookup(&self, name: &str) -> Result<DirEntry> {
        let initramfs_inode = self.files.get(name).ok_or(Error::new(Errno::ENOENT))?;
        let inode = match initramfs_inode {
            InitramFsINode::File(file) => INode::FileLike(file.clone() as Arc<dyn FileLike>),
            InitramFsINode::Directory(dir) => INode::Directory(dir.clone() as Arc<dyn Directory>),
        };

        Ok(DirEntry { inode })
    }
}

pub struct InitramFs {
    root_dir: Arc<InitramFsDir>,
}

impl InitramFs {
    pub fn new(fs_image: &'static [u8]) -> InitramFs {
        let mut image = BytesParser::new(fs_image);
        let mut root_files = HashMap::new();
        loop {
            let magic = parse_hex_field(image.consume_bytes(6).unwrap());
            if magic != 0x070701 {
                panic!(
                    "initramfs: invalid magic (expected {:x}, got {:x})",
                    0x070701, magic
                );
            }

            let ino = parse_hex_field(image.consume_bytes(8).unwrap());
            let mode = FileMode::new(parse_hex_field(image.consume_bytes(8).unwrap()) as u32);
            let uid = parse_hex_field(image.consume_bytes(8).unwrap());
            let gid = parse_hex_field(image.consume_bytes(8).unwrap());
            let nlink = parse_hex_field(image.consume_bytes(8).unwrap());
            let mtime = parse_hex_field(image.consume_bytes(8).unwrap());
            let filesize = parse_hex_field(image.consume_bytes(8).unwrap());
            let dev_major = parse_hex_field(image.consume_bytes(8).unwrap());
            let dev_minor = parse_hex_field(image.consume_bytes(8).unwrap());

            // Skip c_rmaj and c_rmin.
            image.skip(16).unwrap();

            let path_len = parse_hex_field(image.consume_bytes(8).unwrap());
            assert!(path_len > 0);

            // Skip checksum.
            image.skip(8).unwrap();

            let mut path = parse_str_field(image.consume_bytes(path_len - 1).unwrap());
            if path.starts_with("./") {
                path = &path[1..];
            }
            if path == "TRAILER!!!" {
                break;
            }

            assert!(!path.is_empty());
            println!("initramfs: \"{}\" ({})", path, ByteSize::new(filesize));

            // Skip the trailing '\0'.
            image.skip(1).unwrap();
            image.skip_until_alignment(4);

            // Look for the parent directory for the file.
            let mut files = &mut root_files;
            let mut filename = None;
            let mut components = Path::new(path).components().peekable();
            while let Some(comp) = components.next() {
                if components.peek().is_none() {
                    filename = Some(comp);
                    break;
                }

                match files.get_mut(comp) {
                    Some(InitramFsINode::Directory(dir)) => {
                        files = &mut Arc::get_mut(dir).unwrap().files;
                    }
                    Some(_) => {
                        panic!(
                            "initramfs: invalid path '{}' ('{}' is not a directory)",
                            path, comp
                        );
                    }
                    None => {
                        panic!(
                            "initramfs: invalid path '{}' ('{}' does not exist)",
                            path, comp
                        );
                    }
                }
            }

            // Create a file or a directory under its parent.
            if mode.is_directory() {
                files.insert(
                    filename.unwrap(),
                    InitramFsINode::Directory(Arc::new(InitramFsDir {
                        files: HashMap::new(),
                        stat: Stat {
                            inode_no: INodeNo::new(ino),
                            mode,
                        },
                    })),
                );
            } else if mode.is_regular_file() {
                let data = image.consume_bytes(filesize).unwrap();
                files.insert(
                    filename.unwrap(),
                    InitramFsINode::File(Arc::new(InitramFsFile {
                        data,
                        stat: Stat {
                            inode_no: INodeNo::new(ino),
                            mode,
                        },
                    })),
                );
            }
        }

        InitramFs {
            root_dir: Arc::new(InitramFsDir {
                // TODO: Should we use other value for the root directory?
                stat: Stat {
                    inode_no: INodeNo::new(2),
                    mode: FileMode::new(S_IFDIR | 0o755),
                },
                files: root_files,
            }),
        }
    }
}

impl FileSystem for InitramFs {
    fn root_dir(&self) -> Result<Arc<dyn Directory>> {
        Ok(self.root_dir.clone())
    }
}

pub fn init() {
    INITRAM_FS.init(|| Arc::new(InitramFs::new(include_bytes!("../../initramfs.bin"))));
}