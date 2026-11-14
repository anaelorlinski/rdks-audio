#!/usr/bin/env python3
"""Makes the fixtures of `tests/fixtures/`: 0.5 s of pink noise encoded by Apple's AAC encoder (`afconvert`), ffmpeg's
and LAME, and the Apple file edited in place to state other things (the cases of DESIGN D2); MP3s with each kind of
header frame, a failed LAME checksum and broken ID3 frames (DESIGN D4 to D6). macOS only (afconvert); needs ffmpeg 8
and lame 3.100 on the PATH.

    python3 -I tools/fixtures.py

The stream priming: 2112 in the afconvert files, 1024 in the ffmpeg ones. Edits never move sample data: the Apple
file's `moov` grows into the `free` box after it.
"""

import os
import struct
import subprocess
import sys
import tempfile

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests", "fixtures")
RATE = 44100
SECS = 0.5
# the Apple file's edit list states this priming (it really has 2112)
EDIT_PRIMING = 1024


def run(*cmd):
    subprocess.run(cmd, check=True)


def top_boxes(b):
    i, out = 0, []
    while i + 8 <= len(b):
        n, t = struct.unpack(">I4s", b[i : i + 8])
        out.append((i, n, t))
        i += n
    return out


def at(b, kind, start=0):
    i = b.find(kind, start)
    assert i >= 4, kind
    return i - 4


def size_of(b, o):
    return struct.unpack(">I", b[o : o + 4])[0]


def grow(b, insert_at, data, parents):
    """Inserts `data` inside `moov` and takes the same bytes from the `free` box after it."""
    names = [t for _, _, t in top_boxes(b)]
    m = names.index(b"moov")
    assert names[m + 1] == b"free", names
    free_at, free_n, _ = top_boxes(b)[m + 1]
    assert free_n - 8 >= len(data), "free box too small"
    b = bytearray(b[:insert_at] + data + b[insert_at:])
    for o in parents:
        struct.pack_into(">I", b, o, size_of(b, o) + len(data))
    f = free_at + len(data)
    struct.pack_into(">I", b, f, free_n - len(data))
    del b[f + free_n - len(data) : f + free_n]
    return bytes(b)


def with_tag_priming(b, priming):
    k = b.find(b"iTunSMPB")
    t = b.find(b" 00000000 ", k)
    old = b[t + 10 : t + 18]
    assert old == b"00000840", old
    return b[: t + 10] + b"%08X" % priming + b[t + 18 :]


def without_tag(b):
    k = b.find(b"iTunSMPB")
    assert k > 0
    return b[:k] + b"iTunXXXX" + b[k + 8 :]


def with_edit(b, media_time):
    moov = at(b, b"moov")
    mvhd = at(b, b"mvhd", moov)
    scale = struct.unpack(">I", b[mvhd + 20 : mvhd + 24])[0]
    trak = at(b, b"trak", moov)
    tkhd = at(b, b"tkhd", trak)
    valid = int(RATE * SECS)
    elst = struct.pack(">I4sII", 28, b"elst", 0, 1) + struct.pack(">IiI", valid * scale // RATE, media_time, 0x10000)
    edts = struct.pack(">I4s", 8 + len(elst), b"edts") + elst
    return grow(b, tkhd + size_of(b, tkhd), edts, [moov, trak])


def with_roll(b, ffmpeg_file):
    src = open(ffmpeg_file, "rb").read()
    s = at(src, b"sgpd")
    roll = bytearray(src[s : s + size_of(src, s)])
    g = at(src, b"sbgp")
    sbgp = bytearray(src[g : g + size_of(src, g)])
    assert sbgp[12:16] == b"roll" and struct.unpack(">I", sbgp[16:20])[0] == 1
    moov = at(b, b"moov")
    trak = at(b, b"trak", moov)
    mdia = at(b, b"mdia", trak)
    minf = at(b, b"minf", mdia)
    stbl = at(b, b"stbl", minf)
    stsz = at(b, b"stsz", stbl)
    samples = struct.unpack(">I", b[stsz + 16 : stsz + 20])[0]
    struct.pack_into(">I", sbgp, 20, samples)
    return grow(b, stbl + size_of(b, stbl), bytes(roll + sbgp), [moov, trak, mdia, minf, stbl])


def syncsafe(n):
    return bytes([(n >> 21) & 127, (n >> 14) & 127, (n >> 7) & 127, n & 127])


def id3_frame(kind, data):
    return kind + struct.pack(">I", len(data)) + b"\0\0" + data


def with_broken_id3(mp3):
    """An ID3v2.3 tag with a good title, then frames shorter than their fixed fields or with a bad text encoding."""
    frames = [
        id3_frame(b"TIT2", b"\0Title"),
        id3_frame(b"COMM", b"\0e"),
        id3_frame(b"PCNT", b"\0\5"),
        id3_frame(b"TPE1", b"\7Artist"),
    ]
    body = b"".join(frames) + b"\0" * 32
    return b"ID3\3\0\0" + syncsafe(len(body)) + body + mp3


def lame_at(mp3):
    k = mp3.find(b"LAME", 0, 2000)
    assert k > 0
    return k


def with_bad_checksum(mp3):
    """The LAME extension's checksum flipped: its values stay right."""
    k = lame_at(mp3)
    b = bytearray(mp3)
    b[k + 35] ^= 0xFF
    return bytes(b)


def without_lame(mp3):
    """The LAME extension's encoder name changed: a Xing header that states no delay."""
    k = lame_at(mp3)
    return mp3[:k] + b"XXXX" + mp3[k + 4 :]


def main():
    if sys.platform != "darwin":
        sys.exit("afconvert is macOS only")
    os.makedirs(OUT, exist_ok=True)
    p = lambda n: os.path.join(OUT, n)
    noise = f"anoisesrc=color=pink:seed=81:duration={SECS}:sample_rate={RATE}:amplitude=0.3"
    run("ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", noise, "-ac", "2", "-c:a", "pcm_s16le", p("src.wav"))
    run("afconvert", "-f", "m4af", "-d", "aac", "-b", "128000", p("src.wav"), p("a_itunes.m4a"))
    run("afconvert", "-f", "m4af", "-d", "aach", "-b", "64000", p("src.wav"), p("he1.m4a"))
    run("afconvert", "-f", "m4af", "-d", "aacp", "-b", "32000", p("src.wav"), p("he2.m4a"))
    run("afconvert", "-f", "m4af", "-d", "alac", p("src.wav"), p("alac.m4a"))
    # raw ADTS streams (.aac): no container statements
    run("afconvert", "-f", "adts", "-d", "aac", "-b", "128000", p("src.wav"), p("adts_lc.aac"))
    run("afconvert", "-f", "adts", "-d", "aach", "-b", "64000", p("src.wav"), p("adts_he.aac"))
    ff = ["ffmpeg", "-v", "error", "-y", "-i", p("src.wav"), "-c:a", "aac", "-b:a", "128k", "-map_metadata", "-1"]
    run(*ff, "-fflags", "+bitexact", p("b_ffmpeg.m4a"))
    run(*ff, "-fflags", "+bitexact", "-use_editlist", "0", p("c_noedit.m4a"))
    run(*ff, "-fflags", "+bitexact", "-f", "adts", p("adts_ffmpeg.aac"))
    a = open(p("a_itunes.m4a"), "rb").read()
    h = with_edit(a, EDIT_PRIMING)
    i = without_tag(h)
    files = {
        "g_tag1024.m4a": with_tag_priming(a, EDIT_PRIMING),
        "h_edit_tag.m4a": h,
        "i_edit.m4a": i,
        "j_edit_roll.m4a": with_roll(i, p("b_ffmpeg.m4a")),
        "k_edit_roll_tag.m4a": with_roll(h, p("b_ffmpeg.m4a")),
    }
    for name, data in files.items():
        assert len(data) == len(a)
        open(p(name), "wb").write(data)
    noise22 = noise.replace(f"sample_rate={RATE}", "sample_rate=22050")
    run("ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", noise22, "-ac", "2", "-c:a", "pcm_s16le", p("src22.wav"))
    lame = ["lame", "--silent", "--noreplaygain"]
    run(*lame, "-b", "128", p("src.wav"), p("mp3_lame_cbr.mp3"))
    run(*lame, "-V", "4", p("src.wav"), p("mp3_lame_vbr.mp3"))
    run(*lame, "-t", "-b", "128", p("src.wav"), p("mp3_noheader.mp3"))
    run(*lame, "-b", "64", p("src22.wav"), p("mp3_mpeg2.mp3"))
    run("ffmpeg", "-v", "error", "-y", "-i", p("src.wav"), "-c:a", "libmp3lame", "-b:a", "128k", "-map_metadata", "-1",
        "-fflags", "+bitexact", p("mp3_ffmpeg.mp3"))
    cbr = open(p("mp3_lame_cbr.mp3"), "rb").read()
    open(p("mp3_broken_id3.mp3"), "wb").write(with_broken_id3(cbr))
    vbr = open(p("mp3_lame_vbr.mp3"), "rb").read()
    open(p("mp3_bad_checksum.mp3"), "wb").write(with_bad_checksum(vbr))
    open(p("mp3_xing_only.mp3"), "wb").write(without_lame(vbr))
    for n in sorted(os.listdir(OUT)):
        print(f"{os.path.getsize(p(n)):7d}  {n}")


if __name__ == "__main__":
    main()
