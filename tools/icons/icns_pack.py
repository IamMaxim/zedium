import sys, struct

# Assemble a valid ICNS container from pre-rendered PNGs.
# Usage: icns_pack.py out.icns OSTYPE:path.png [OSTYPE:path.png ...]
out = sys.argv[1]
chunks = b""
for pair in sys.argv[2:]:
    ostype, path = pair.split(":", 1)
    with open(path, "rb") as handle:
        data = handle.read()
    chunks += ostype.encode("ascii") + struct.pack(">I", len(data) + 8) + data
body = b"icns" + struct.pack(">I", len(chunks) + 8) + chunks
with open(out, "wb") as handle:
    handle.write(body)
print("wrote", out, len(body), "bytes,", len(sys.argv) - 2, "images")
