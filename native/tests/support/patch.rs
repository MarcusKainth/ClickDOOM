//! A patch and texture decoder, for the tests to check the SQL against.
//!
//! This is an oracle and nothing else. Native mode decodes patches and
//! composes textures in SQL; a Rust decoder on the load path would be the
//! precomputed answer `PURITY.md` rules out. Here it is a second
//! implementation, written from `r_data.c` rather than from the SQL, so
//! the two agreeing means something.

use clickdoom_native::wad::Wad;

/// One post of a patch column.
#[derive(Clone, Copy, Debug)]
pub struct Post {
    pub topdelta: u8,
    pub length: u8,
    /// Where the post's pixels start in the lump.
    pub data_at: usize,
}

/// A lump in `patch_t` form.
pub struct Patch<'a> {
    pub width: u16,
    pub height: u16,
    pub columnofs: Vec<u32>,
    pub bytes: &'a [u8],
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn i16_at(bytes: &[u8], at: usize) -> i16 {
    u16_at(bytes, at) as i16
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

impl<'a> Patch<'a> {
    pub fn parse(bytes: &'a [u8]) -> Patch<'a> {
        let width = u16_at(bytes, 0);
        let columnofs = (0..width as usize)
            .map(|c| u32_at(bytes, 8 + c * 4))
            .collect();
        Patch {
            width,
            height: u16_at(bytes, 2),
            columnofs,
            bytes,
        }
    }

    /// The posts of one column, in chain order.
    pub fn posts(&self, col: usize) -> Vec<Post> {
        let mut at = self.columnofs[col] as usize;
        let mut posts = Vec::new();
        while at < self.bytes.len() && self.bytes[at] != 0xff {
            let length = self.bytes[at + 1];
            posts.push(Post {
                topdelta: self.bytes[at],
                length,
                data_at: at + 3,
            });
            at += length as usize + 4;
        }
        posts
    }
}

/// One entry of TEXTURE1.
pub struct Texture {
    pub name: String,
    pub width: u16,
    pub height: u16,
    /// `(originx, originy, pnames index)` per patch, in TEXTURE1's order.
    pub patches: Vec<(i16, i16, usize)>,
}

/// PNAMES, upper-cased the way `W_CheckNumForName`'s comparison is.
pub fn pnames(wad: &Wad<'_>) -> Vec<String> {
    let bytes = wad.find("PNAMES").expect("PNAMES").bytes;
    let count = u32_at(bytes, 0) as usize;
    (0..count)
        .map(|i| name_at(&bytes[4 + i * 8..4 + i * 8 + 8]))
        .collect()
}

fn name_at(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).to_uppercase()
}

/// TEXTURE1, in its own order.
pub fn textures(wad: &Wad<'_>) -> Vec<Texture> {
    let bytes = wad.find("TEXTURE1").expect("TEXTURE1").bytes;
    let count = u32_at(bytes, 0) as usize;
    (0..count)
        .map(|i| {
            let at = u32_at(bytes, 4 + i * 4) as usize;
            let patchcount = u16_at(bytes, at + 20) as usize;
            Texture {
                name: name_at(&bytes[at..at + 8]),
                width: u16_at(bytes, at + 12),
                height: u16_at(bytes, at + 14),
                patches: (0..patchcount)
                    .map(|j| {
                        let p = at + 22 + j * 10;
                        (
                            i16_at(bytes, p),
                            i16_at(bytes, p + 2),
                            u16_at(bytes, p + 4) as usize,
                        )
                    })
                    .collect(),
            }
        })
        .collect()
}

/// `R_GenerateLookup`'s per-column result: how many patches cover it, the
/// lump it reads from when one does, and the offset into that lump or into
/// the composite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Column {
    pub patches: u16,
    pub lump: i64,
    pub ofs: u32,
}

/// `R_GenerateLookup` and `R_GenerateComposite` for one texture.
///
/// Returns the per-column lookup and the composite buffer, which is empty
/// when no column needs composing.
pub fn compose(wad: &Wad<'_>, texture: &Texture, pnames: &[String]) -> (Vec<Column>, Vec<u8>) {
    let width = texture.width as usize;
    let height = texture.height as usize;
    let lump_of =
        |patch: usize| -> i64 { wad.find(&pnames[patch]).map_or(-1, |l| i64::from(l.index)) };

    let mut count = vec![0u16; width];
    let mut single = vec![(0i64, 0u32); width];
    for (originx, _, patch) in &texture.patches {
        let lump = lump_of(*patch);
        let bytes = wad.lumps()[lump as usize].bytes;
        let p = Patch::parse(bytes);
        for c in 0..p.width as i32 {
            let x = *originx as i32 + c;
            if !(0..width as i32).contains(&x) {
                continue;
            }
            if count[x as usize] == 0 {
                single[x as usize] = (lump, p.columnofs[c as usize] + 3);
            }
            count[x as usize] += 1;
        }
    }

    let mut columns = Vec::with_capacity(width);
    let mut size = 0u32;
    for x in 0..width {
        columns.push(match count[x] {
            1 => Column {
                patches: 1,
                lump: single[x].0,
                ofs: single[x].1,
            },
            n => {
                let column = Column {
                    patches: n,
                    lump: -1,
                    ofs: size,
                };
                size += height as u32;
                column
            }
        });
    }
    if size == 0 {
        return (columns, Vec::new());
    }

    let mut block = vec![0u8; size as usize];
    for (originx, originy, patch) in &texture.patches {
        let lump = lump_of(*patch);
        let bytes = wad.lumps()[lump as usize].bytes;
        let p = Patch::parse(bytes);
        for c in 0..p.width as i32 {
            let x = *originx as i32 + c;
            if !(0..width as i32).contains(&x) || columns[x as usize].lump >= 0 {
                continue;
            }
            let at = columns[x as usize].ofs as usize;
            for post in p.posts(c as usize) {
                // `R_DrawColumnInCache`'s clipping. The source pointer
                // does not move when the top is cut, which shifts the
                // pixels up rather than dropping them.
                let mut n = i32::from(post.length);
                let mut position = i32::from(*originy) + i32::from(post.topdelta);
                if position < 0 {
                    n += position;
                    position = 0;
                }
                if position + n > height as i32 {
                    n = height as i32 - position;
                }
                if n <= 0 {
                    continue;
                }
                let (n, position) = (n as usize, position as usize);
                block[at + position..at + position + n]
                    .copy_from_slice(&bytes[post.data_at..post.data_at + n]);
            }
        }
    }
    (columns, block)
}

/// The 128 bytes a draw reads from a column, and whether the source ran
/// out before them.
pub fn window(source: &[u8], ofs: u32) -> (Vec<u8>, bool) {
    let at = (ofs as usize).min(source.len());
    let end = (at + 128).min(source.len());
    let mut window = source[at..end].to_vec();
    let overrun = window.len() < 128;
    window.resize(128, 0);
    (window, overrun)
}
