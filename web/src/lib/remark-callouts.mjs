// :::tip / :::note / :::warning blocks (a paragraph that starts with ":::name" and ends with ":::").
export default function remarkCallouts() {
  const walk = (node) => {
    if (!node.children) return;
    for (const child of node.children) {
      if (child.type === "paragraph" && child.children?.[0]?.type === "text") {
        const first = child.children[0];
        const m = /^:::(tip|note|warning)\n/.exec(first.value);
        const last = child.children[child.children.length - 1];
        if (m && last.type === "text" && /\n:::$/.test(last.value)) {
          first.value = first.value.slice(m[0].length);
          last.value = last.value.replace(/\n:::$/, "");
          child.data = { hName: "div", hProperties: { className: ["callout", `callout-${m[1]}`], role: "note" } };
          child.children = [{ type: "paragraph", children: child.children }];
        }
      }
      walk(child);
    }
  };
  return (tree) => walk(tree);
}
