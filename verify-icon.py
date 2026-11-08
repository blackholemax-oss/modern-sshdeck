"""Independently verify which icon is embedded in a PE file.

Approach: locate the .rsrc section and dump ALL RT_GROUP_ICON entries,
then extract every RT_ICON frame and report its byte size + first bytes.
Compare against the on-disk icon.ico frames.

This avoids trusting a single parse path.
"""
import hashlib
import struct
import sys

PE = sys.argv[1] if len(sys.argv) > 1 else (
    "src-tauri/target/x86_64-pc-windows-msvc/release-fast/modern-sshdeck.exe"
)
ICO = "src-tauri/icons/icon.ico"


def read_pe(path):
    d = open(path, "rb").read()
    pe = struct.unpack("<I", d[0x3C:0x40])[0]
    assert d[pe:pe + 4] == b"PE\0\0"
    nsec = struct.unpack("<H", d[pe + 6:pe + 8])[0]
    optsz = struct.unpack("<H", d[pe + 20:pe + 22])[0]
    oo = pe + 24
    magic = struct.unpack("<H", d[oo:oo + 2])[0]
    dirs = oo + (112 if magic == 0x20B else 96)
    res_rva = struct.unpack("<I", d[dirs + 2 * 8:dirs + 2 * 8 + 4])[0]
    secoff = oo + optsz
    secs = []
    for i in range(nsec):
        s = secoff + i * 40
        nm = d[s:s + 8].rstrip(b"\0").decode("ascii", "ignore")
        vs, va = struct.unpack("<II", d[s + 8:s + 16])
        rs, rp = struct.unpack("<II", d[s + 16:s + 24])
        secs.append((va, vs, rp, rs, nm))
    tres = None
    for va, vs, rp, rs, nm in secs:
        if va <= res_rva < va + max(vs, rs):
            tres = rp + (res_rva - va)
    return d, secs, tres


def r2o(rva, secs):
    for va, vs, rp, rs, nm in secs:
        if va <= rva < va + max(vs, rs):
            return rp + (rva - va)
    return None


def entries(d, tres, off):
    _c, _t, _ma, _mi, nn, nid = struct.unpack("<IIHHHH", d[off:off + 16])
    out = []
    for i in range(nid):
        e = off + 16 + i * 8
        nidv, ptr = struct.unpack("<II", d[e:e + 8])
        if nidv & 0x80000000:
            continue  # named entry, skip
        out.append((nidv, ptr))
    return out


def main():
    d, secs, tres = read_pe(PE)
    print("PE:", PE, "rsrc off=0x%x" % tres)

    root = dict(entries(d, tres, tres))
    print("root types:", sorted(root.keys()))

    # ---- disk ico frames ----
    dd = open(ICO, "rb").read()
    _r, _t, cnt = struct.unpack("<HHH", dd[:6])
    disk = []
    off = 6
    for _ in range(cnt):
        w, h, _col, _rv, _pl, _bp, sz, ofs = struct.unpack("<BBBBHHII", dd[off:off + 16])
        off += 16
        disk.append((w or 256, h or 256, sz, hashlib.sha256(dd[ofs:ofs + sz]).hexdigest()[:16]))
    print("\n-- disk icon.ico frames --")
    for f in disk:
        print("   %dx%d bytes=%-6d sha=%s" % f)

    # ---- pe group icon ----
    gname = tres + (root[14] & 0x7FFFFFFF)
    ids = entries(d, tres, gname)
    gid_dir = tres + (ids[0][1] & 0x7FFFFFFF)
    langs = entries(d, tres, gid_dir)
    gdata_off = tres + (langs[0][1] & 0x7FFFFFFF)
    grva, gsize = struct.unpack("<II", d[gdata_off:gdata_off + 8])
    grp = d[r2o(grva, secs):][:gsize]

    _r2, _t2, gcnt = struct.unpack("<HHH", grp[:6])
    print("\n-- PE embedded GRPICON count=%d --" % gcnt)
    peframes = []
    for i in range(gcnt):
        e = 6 + i * 14
        w, h, _col, _rv, _pl, _bp, sz, rid = struct.unpack("<BBBBHHIH", grp[e:e + 14])
        peframes.append((w or 256, h or 256, sz, rid))
    for f in peframes:
        print("   %dx%d decl_bytes=%-6d id=%d" % f)

    # ---- extract each RT_ICON and hash ----
    icon_type = tres + (root[3] & 0x7FFFFFFF)
    icon_ids = {nid: ptr for nid, ptr in entries(d, tres, icon_type)}

    print("\n-- PE RT_ICON actual bytes --")
    pe_hashes = []
    for W, H, _sz, rid in sorted(peframes, key=lambda x: -x[0]):
        ptr = icon_ids.get(rid)
        if ptr is None:
            print("   id=%d MISSING" % rid)
            continue
        lang_dir = tres + (ptr & 0x7FFFFFFF)
        le = entries(d, tres, lang_dir)
        de = tres + (le[0][1] & 0x7FFFFFFF)
        rva, size = struct.unpack("<II", d[de:de + 8])
        blob = d[r2o(rva, secs):][:size]
        hsh = hashlib.sha256(blob).hexdigest()[:16]
        pe_hashes.append((W, H, size, hsh))
        print("   %dx%d bytes=%-6d sha=%s sig=%s" % (W, H, size, hsh, blob[:4]))

    # ---- compare ----
    print("\n=== COMPARE (by size) ===")
    disk_by_size = {s: h for w, h_, s, h in disk}
    same = 0
    for W, H, size, hsh in pe_hashes:
        dh = disk_by_size.get(size)
        if dh is None:
            print("   %dx%d size=%d -> NO disk frame with same size" % (W, H, size))
        elif dh == hsh:
            same += 1
            print("   %dx%d size=%d -> MATCH disk (%s)" % (W, H, size, hsh))
        else:
            print("   %dx%d size=%d -> DIFFERENT content!" % (W, H, size))
    print("\nmatched frames: %d / %d" % (same, len(pe_hashes)))


main()
