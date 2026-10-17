// Address tokens accept plain addresses and the usual Display Name <address> form.
export function parseAddresses(value) {
  if (!value.trim()) return [];
  const tokens = value.match(/(?:"[^"]*"|[^,;])+/g) || [];
  return tokens.map((token) => {
    const text = token.trim();
    const match = text.match(/<([^<>]+)>$/);
    const address = (match ? match[1] : text).trim();
    if (!/^[^\s@<>]+@[^\s@<>]+$/.test(address)) throw new Error("address");
    return address;
  });
}
export function safeLink(url) {
  try {
    return ["https:", "http:", "mailto:"].includes(new URL(url).protocol);
  } catch {
    return false;
  }
}
export function fileSize(bytes, language) {
  return new Intl.NumberFormat(language, {
    style: "unit",
    unit: bytes >= 1024 * 1024 ? "megabyte" : "kilobyte",
    maximumFractionDigits: 1,
  }).format(bytes / (bytes >= 1024 * 1024 ? 1024 * 1024 : 1024));
}
