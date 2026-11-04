#!/usr/bin/env bash
# Runs every face of a built doc-sanitize once on files made here: smoke.sh <binary>
set -eu
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
cd "$work"

# A PDF with an author, and a Word file with one.
cat > filing.pdf <<'PDF'
%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>
endobj
4 0 obj
<< /Author (Avery Example) >>
endobj
trailer
<< /Root 1 0 R /Info 4 0 R >>
%%EOF
PDF
for python in python3 python py; do
  "$python" -c '' 2>/dev/null && break
done
"$python" - <<'PY'
import zipfile
parts = {
    "[Content_Types].xml": '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>',
    "_rels/.rels": '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>',
    "word/document.xml": '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>',
    "docProps/core.xml": '<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>Avery Example</dc:creator></cp:coreProperties>',
}
with zipfile.ZipFile("memo.docx", "w", zipfile.ZIP_DEFLATED) as z:
    for name, text in parts.items():
        z.writestr(name, text)
PY

# check finds the author, clean removes it, and the copies have nothing left.
for f in filing.pdf memo.docx; do
  code=0
  "$bin" check "$f" --json > report.json || code=$?
  test $code = 1
  grep -q '"status": "found"' report.json
  "$bin" clean "$f" --json > cleaned.json
  grep -q '"status": "cleaned"' cleaned.json
done
"$bin" filing.clean.pdf memo.clean.docx
if grep -qa 'Avery Example' filing.clean.pdf; then echo 'the author is still in the copy'; exit 1; fi

# mcp answers initialize, lists both tools and checks a file, then leaves when stdin closes.
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"check_document","arguments":{"paths":["filing.pdf"]}}}'
} | "$bin" mcp > mcp.out
grep -q '"name":"doc-sanitize"' mcp.out
grep -q '"name":"clean_document"' mcp.out
grep -q 'structuredContent' mcp.out

# ui serves the page and checks an upload, then a request from another site is refused.
"$bin" ui --no-open > ui.out &
ui=$!
trap 'kill $ui 2>/dev/null || true' EXIT
for _ in $(seq 50); do
  grep -q '^Open ' ui.out && break
  sleep 0.1
done
url=$(sed -n 's/^Open \(http:[^ ]*\).*/\1/p' ui.out)
curl -fsS "$url/" | grep -q 'Find and remove hidden data'
curl -fsS -H 'X-File-Name: filing.pdf' --data-binary @filing.pdf "$url/api/check" | grep -q '"status":"found"'
curl -fsS -D headers.txt -o copy.pdf -H 'X-File-Name: filing.pdf' --data-binary @filing.pdf "$url/api/clean"
grep -qi '^x-result:' headers.txt
test "$(curl -s -o /dev/null -w '%{http_code}' -H 'Origin: http://evil.test' "$url/")" = 403
echo 'every face answered'
