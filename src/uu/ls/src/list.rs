pub fn list(locs: Vec<String>, config: Config) -> i32 {
    let mut files = Vec::<PathData>::new();
    let mut dirs = Vec::<PathData>::new();
    let mut has_failed = false;

    let mut out = BufWriter::new(stdout());

    for loc in &locs {
        let p = PathBuf::from(&loc);
        if !p.exists() {
            show_error!("'{}': {}", &loc, "No such file or directory");
            /*
            We found an error, the return code of ls should not be 0
            And no need to continue the execution
            */
            has_failed = true;
            continue;
        }

        let path_data = PathData::new(p, None, None, &config, true);

        let show_dir_contents = match path_data.file_type() {
            Some(ft) => !config.directory && ft.is_dir(),
            None => {
                has_failed = true;
                false
            }
        };

        if show_dir_contents {
            dirs.push(path_data);
        } else {
            files.push(path_data);
        }
    }
    sort_entries(&mut files, &config);
    display_items(&files, &config, &mut out);

    sort_entries(&mut dirs, &config);
    for dir in dirs {
        if locs.len() > 1 || config.recursive {
            writeln!(out, "\n{}:", dir.p_buf.display()).unwrap();
        }
        enter_directory(&dir, &config, &mut out);
    }
    if has_failed {
        1
    } else {
        0
    }
}

fn sort_entries(entries: &mut Vec<PathData>, config: &Config) {
    match config.sort {
        Sort::Time => entries.sort_by_key(|k| {
            Reverse(
                k.md()
                    .and_then(|md| get_system_time(md, config))
                    .unwrap_or(UNIX_EPOCH),
            )
        }),
        Sort::Size => {
            entries.sort_by_key(|k| Reverse(k.md().as_ref().map(|md| md.len()).unwrap_or(0)))
        }
        // The default sort in GNU ls is case insensitive
        Sort::Name => entries.sort_by(|a, b| a.display_name.cmp(&b.display_name)),
        Sort::Version => entries.sort_by(|a, b| version_cmp::version_cmp(&a.p_buf, &b.p_buf)),
        Sort::Extension => entries.sort_by(|a, b| {
            a.p_buf
                .extension()
                .cmp(&b.p_buf.extension())
                .then(a.p_buf.file_stem().cmp(&b.p_buf.file_stem()))
        }),
        Sort::None => {}
    }

    if config.reverse {
        entries.reverse();
    }
}

#[cfg(windows)]
fn is_hidden(file_path: &DirEntry) -> bool {
    let metadata = fs::metadata(file_path.path()).unwrap();
    let attr = metadata.file_attributes();
    (attr & 0x2) > 0
}

fn should_display(entry: &DirEntry, config: &Config) -> bool {
    let ffi_name = entry.file_name();

    // For unix, the hidden files are already included in the ignore pattern
    #[cfg(windows)]
    {
        if config.files == Files::Normal && is_hidden(entry) {
            return false;
        }
    }

    !config.ignore_patterns.is_match(&ffi_name)
}

fn enter_directory(dir: &PathData, config: &Config, out: &mut BufWriter<Stdout>) {
    let mut entries: Vec<_> = if config.files == Files::All {
        vec![
            PathData::new(
                dir.p_buf.clone(),
                Some(Ok(*dir.file_type().unwrap())),
                Some(".".into()),
                config,
                false,
            ),
            PathData::new(dir.p_buf.join(".."), None, Some("..".into()), config, false),
        ]
    } else {
        vec![]
    };

    let mut temp: Vec<_> = safe_unwrap!(fs::read_dir(&dir.p_buf))
        .map(|res| safe_unwrap!(res))
        .filter(|e| should_display(e, config))
        .map(|e| PathData::new(DirEntry::path(&e), Some(e.file_type()), None, config, false))
        .collect();

    sort_entries(&mut temp, config);

    entries.append(&mut temp);

    display_items(&entries, config, out);

    if config.recursive {
        for e in entries
            .iter()
            .skip(if config.files == Files::All { 2 } else { 0 })
            .filter(|p| p.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        {
            writeln!(out, "\n{}:", e.p_buf.display()).unwrap();
            enter_directory(e, config, out);
        }
    }
}

fn get_metadata(entry: &Path, dereference: bool) -> std::io::Result<Metadata> {
    if dereference {
        entry.metadata().or_else(|_| entry.symlink_metadata())
    } else {
        entry.symlink_metadata()
    }
}

fn pad_left(string: String, count: usize) -> String {
    format!("{:>width$}", string, width = count)
}

fn display_items(items: &[PathData], config: &Config, out: &mut BufWriter<Stdout>) {
    if config.format == Format::Long {
        display_item_configurable(items, config, out);
    } else {
        let names = items.iter().filter_map(|i| display_file_name(i, config));

        match (&config.format, config.width) {
            (Format::Columns, Some(width)) => {
                display_grid(names, width, Direction::TopToBottom, out);
            }
            (Format::Across, Some(width)) => {
                display_grid(names, width, Direction::LeftToRight, out);
            }
            (Format::Commas, width_opt) => {
                display_comma(names, items.len(), width_opt.unwrap_or(1).into(), out);
            }
            _ => {
                for name in names {
                    writeln!(out, "{}", name.contents).unwrap();
                }
            }
        }
    }
}

fn get_block_size(md: &Metadata, config: &Config) -> u64 {
    /* GNU ls will display sizes in terms of block size
       md.len() will differ from this value when the file has some holes
    */
    #[cfg(unix)]
    {
        // hard-coded for now - enabling setting this remains a TODO
        let ls_block_size = 1024;
        match config.size_format {
            SizeFormat::Binary => md.blocks() * 512,
            SizeFormat::Decimal => md.blocks() * 512,
            SizeFormat::Bytes => md.blocks() * 512 / ls_block_size,
        }
    }

    #[cfg(not(unix))]
    {
        let _ = config;
        // no way to get block size for windows, fall-back to file size
        md.len()
    }
}

fn display_comma(
    names: impl Iterator<Item = Cell>,
    length: usize,
    width: usize,
    out: &mut BufWriter<Stdout>,
) {
    let mut current_col = 0;
    for (idx, name) in names.enumerate() {
        let name_width = name.width;
        if current_col + name_width + 1 /* comma */ > width {
            current_col = 0;
            writeln!(out).unwrap();
        }
        // Last element
        if idx.saturating_add(1) == length {
            writeln!(out, "{}", name.contents).unwrap();
        } else {
            write!(out, "{}, ", name.contents).unwrap();
        }
        current_col += name_width + 1 /* comma */ + 1 /* whitespace */;
    }
}

fn display_grid(
    names: impl Iterator<Item = Cell>,
    width: u16,
    direction: Direction,
    out: &mut BufWriter<Stdout>,
) {
    let mut grid = Grid::new(GridOptions {
        filling: Filling::Spaces(2),
        direction,
    });

    for name in names {
        grid.add(name);
    }

    match grid.fit_into_width(width.into()) {
        Some(output) => {
            write!(out, "{}", output).unwrap();
        }
        // Width is too small for the grid, so we fit it in one column
        None => {
            write!(out, "{}", grid.fit_into_columns(1)).unwrap();
        }
    }
}

fn display_item_configurable(paths: &[PathData], config: &Config, out: &mut BufWriter<Stdout>) {
    let mut result = String::new();
    let metadatas = paths
        .iter()
        // FIXME: For now it just unwraps. Perhaps introduce better error messages?
        .map(|path| path.md().unwrap())
        .collect::<Vec<_>>();
    let (max_links, max_width, total_size) = metadatas
        .iter()
        .map(|md| {
            (
                display_symlink_count(md).len(),
                display_size_or_rdev(md, config).len(),
                get_block_size(md, config),
            )
        })
        .fold((1, 1, 0u64), |(a, b, c), (d, e, f)| {
            (a.max(d), b.max(e), c.saturating_add(f))
        });
    for (item, md) in paths.iter().zip(metadatas.iter()) {
        #[cfg(unix)]
        {
            if config.inode {
                result.push_str(&format!("{} ", get_inode(md)));
            }
        }
        result.push_str(&format!(
            "{} {}",
            display_permissions(md, true),
            pad_left(display_symlink_count(md), max_links),
        ));
        if config.long.owner {
            result.push_str(&format!(" {}", display_uname(md, config)));
        }
        if config.long.group {
            result.push_str(&format!(" {}", display_group(md, config)));
        }
        // Author is only different from owner on GNU/Hurd, so we reuse
        // the owner, since GNU/Hurd is not currently supported by Rust.
        if config.long.author {
            result.push_str(&format!(" {}", display_uname(md, config)));
        }
        result.push_str(&format!(
            " {} {} {}",
            pad_left(display_size_or_rdev(md, config), max_width),
            display_date(md, config),
            // unwrap is fine because it fails when metadata is not available
            // but we already know that it is because it's checked at the
            // start of the function.
            display_file_name(item, config).unwrap().contents,
        ));
        result.push('\n');
    }
    if total_size > 0 {
        writeln!(out, "total {}", display_size(total_size, config)).unwrap();
    }
    write!(out, "{}", result.as_str()).unwrap();
}

#[cfg(unix)]
fn get_inode(metadata: &Metadata) -> String {
    format!("{:8}", metadata.ino())
}

// Currently getpwuid is `linux` target only. If it's broken out into
// a posix-compliant attribute this can be updated...
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use uucore::entries;
use uucore::{fs::display_permissions, InvalidEncodingHandling};

use crate::text::{ABOUT, AFTER_HELP, NONE_HELP};

#[cfg(unix)]
fn cached_uid2usr(uid: u32) -> String {
    lazy_static! {
        static ref UID_CACHE: Mutex<HashMap<u32, String>> = Mutex::new(HashMap::new());
    }

    let mut uid_cache = UID_CACHE.lock().unwrap();
    uid_cache
        .entry(uid)
        .or_insert_with(|| entries::uid2usr(uid).unwrap_or_else(|_| uid.to_string()))
        .clone()
}

#[cfg(unix)]
fn display_uname(metadata: &Metadata, config: &Config) -> String {
    if config.long.numeric_uid_gid {
        metadata.uid().to_string()
    } else {
        cached_uid2usr(metadata.uid())
    }
}

#[cfg(unix)]
fn cached_gid2grp(gid: u32) -> String {
    lazy_static! {
        static ref GID_CACHE: Mutex<HashMap<u32, String>> = Mutex::new(HashMap::new());
    }

    let mut gid_cache = GID_CACHE.lock().unwrap();
    gid_cache
        .entry(gid)
        .or_insert_with(|| entries::gid2grp(gid).unwrap_or_else(|_| gid.to_string()))
        .clone()
}

#[cfg(unix)]
fn display_group(metadata: &Metadata, config: &Config) -> String {
    if config.long.numeric_uid_gid {
        metadata.gid().to_string()
    } else {
        cached_gid2grp(metadata.gid())
    }
}

#[cfg(not(unix))]
fn display_uname(_metadata: &Metadata, _config: &Config) -> String {
    "somebody".to_string()
}

#[cfg(not(unix))]
fn display_group(_metadata: &Metadata, _config: &Config) -> String {
    "somegroup".to_string()
}

// The implementations for get_time are separated because some options, such
// as ctime will not be available
#[cfg(unix)]
fn get_system_time(md: &Metadata, config: &Config) -> Option<SystemTime> {
    match config.time {
        Time::Change => Some(UNIX_EPOCH + Duration::new(md.ctime() as u64, md.ctime_nsec() as u32)),
        Time::Modification => md.modified().ok(),
        Time::Access => md.accessed().ok(),
        Time::Birth => md.created().ok(),
    }
}

#[cfg(not(unix))]
fn get_system_time(md: &Metadata, config: &Config) -> Option<SystemTime> {
    match config.time {
        Time::Modification => md.modified().ok(),
        Time::Access => md.accessed().ok(),
        Time::Birth => md.created().ok(),
        _ => None,
    }
}

fn get_time(md: &Metadata, config: &Config) -> Option<chrono::DateTime<chrono::Local>> {
    let time = get_system_time(md, config)?;
    Some(time.into())
}

fn display_date(metadata: &Metadata, config: &Config) -> String {
    match get_time(metadata, config) {
        Some(time) => {
            //Date is recent if from past 6 months
            //According to GNU a Gregorian year has 365.2425 * 24 * 60 * 60 == 31556952 seconds on the average.
            let recent = time + chrono::Duration::seconds(31_556_952 / 2) > chrono::Local::now();

            match config.time_style {
                TimeStyle::FullIso => time.format("%Y-%m-%d %H:%M:%S.%f %z"),
                TimeStyle::LongIso => time.format("%Y-%m-%d %H:%M"),
                TimeStyle::Iso => time.format(if recent { "%m-%d %H:%M" } else { "%Y-%m-%d " }),
                TimeStyle::Locale => {
                    let fmt = if recent { "%b %e %H:%M" } else { "%b %e  %Y" };

                    // spell-checker:ignore (word) datetime
                    //In this version of chrono translating can be done
                    //The function is chrono::datetime::DateTime::format_localized
                    //However it's currently still hard to get the current pure-rust-locale
                    //So it's not yet implemented

                    time.format(fmt)
                }
            }
            .to_string()
        }
        None => "???".into(),
    }
}

// There are a few peculiarities to how GNU formats the sizes:
// 1. One decimal place is given if and only if the size is smaller than 10
// 2. It rounds sizes up.
// 3. The human-readable format uses powers for 1024, but does not display the "i"
//    that is commonly used to denote Kibi, Mebi, etc.
// 4. Kibi and Kilo are denoted differently ("k" and "K", respectively)
fn format_prefixed(prefixed: NumberPrefix<f64>) -> String {
    match prefixed {
        NumberPrefix::Standalone(bytes) => bytes.to_string(),
        NumberPrefix::Prefixed(prefix, bytes) => {
            // Remove the "i" from "Ki", "Mi", etc. if present
            let prefix_str = prefix.symbol().trim_end_matches('i');

            // Check whether we get more than 10 if we round up to the first decimal
            // because we want do display 9.81 as "9.9", not as "10".
            if (10.0 * bytes).ceil() >= 100.0 {
                format!("{:.0}{}", bytes.ceil(), prefix_str)
            } else {
                format!("{:.1}{}", (10.0 * bytes).ceil() / 10.0, prefix_str)
            }
        }
    }
}

fn display_size_or_rdev(metadata: &Metadata, config: &Config) -> String {
    #[cfg(unix)]
    {
        let ft = metadata.file_type();
        if ft.is_char_device() || ft.is_block_device() {
            let dev: u64 = metadata.rdev();
            let major = (dev >> 8) as u8;
            let minor = dev as u8;
            return format!("{}, {}", major, minor);
        }
    }

    display_size(metadata.len(), config)
}

fn display_size(size: u64, config: &Config) -> String {
    // NOTE: The human-readable behavior deviates from the GNU ls.
    // The GNU ls uses binary prefixes by default.
    match config.size_format {
        SizeFormat::Binary => format_prefixed(NumberPrefix::binary(size as f64)),
        SizeFormat::Decimal => format_prefixed(NumberPrefix::decimal(size as f64)),
        SizeFormat::Bytes => size.to_string(),
    }
}

#[cfg(unix)]
fn file_is_executable(md: &Metadata) -> bool {
    // Mode always returns u32, but the flags might not be, based on the platform
    // e.g. linux has u32, mac has u16.
    // S_IXUSR -> user has execute permission
    // S_IXGRP -> group has execute permission
    // S_IXOTH -> other users have execute permission
    md.mode() & ((S_IXUSR | S_IXGRP | S_IXOTH) as u32) != 0
}

fn classify_file(path: &PathData) -> Option<char> {
    let file_type = path.file_type()?;

    if file_type.is_dir() {
        Some('/')
    } else if file_type.is_symlink() {
        Some('@')
    } else {
        #[cfg(unix)]
        {
            if file_type.is_socket() {
                Some('=')
            } else if file_type.is_fifo() {
                Some('|')
            } else if file_type.is_file() && file_is_executable(path.md()?) {
                Some('*')
            } else {
                None
            }
        }
        #[cfg(not(unix))]
        None
    }
}

fn display_file_name(path: &PathData, config: &Config) -> Option<Cell> {
    let mut name = escape_name(&path.display_name, &config.quoting_style);

    #[cfg(unix)]
    {
        if config.format != Format::Long && config.inode {
            name = get_inode(path.md()?) + " " + &name;
        }
    }

    // We need to keep track of the width ourselves instead of letting term_grid
    // infer it because the color codes mess up term_grid's width calculation.
    let mut width = name.width();

    if let Some(ls_colors) = &config.color {
        name = color_name(ls_colors, &path.p_buf, name, path.md()?);
    }

    if config.indicator_style != IndicatorStyle::None {
        let sym = classify_file(path);

        let char_opt = match config.indicator_style {
            IndicatorStyle::Classify => sym,
            IndicatorStyle::FileType => {
                // Don't append an asterisk.
                match sym {
                    Some('*') => None,
                    _ => sym,
                }
            }
            IndicatorStyle::Slash => {
                // Append only a slash.
                match sym {
                    Some('/') => Some('/'),
                    _ => None,
                }
            }
            IndicatorStyle::None => None,
        };

        if let Some(c) = char_opt {
            name.push(c);
            width += 1;
        }
    }

    if config.format == Format::Long && path.file_type()?.is_symlink() {
        if let Ok(target) = path.p_buf.read_link() {
            name.push_str(" -> ");
            name.push_str(&target.to_string_lossy());
        }
    }

    Some(Cell {
        contents: name,
        width,
    })
}

fn color_name(ls_colors: &LsColors, path: &Path, name: String, md: &Metadata) -> String {
    match ls_colors.style_for_path_with_metadata(path, Some(md)) {
        Some(style) => style.to_ansi_term_style().paint(name).to_string(),
        None => name,
    }
}

#[cfg(not(unix))]
fn display_symlink_count(_metadata: &Metadata) -> String {
    // Currently not sure of how to get this on Windows, so I'm punting.
    // Git Bash looks like it may do the same thing.
    String::from("1")
}

#[cfg(unix)]
fn display_symlink_count(metadata: &Metadata) -> String {
    metadata.nlink().to_string()
}
