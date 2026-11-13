/**
 * One persistent DOM node per layout item id. A widget is portaled into its
 * node from a single keyed list, and the node is appended to whichever zone
 * slot currently shows the item, so moving an item to another zone moves the
 * node, never the React instance.
 */
export type WidgetNodes = {
  /** The node for `id`, created on first use. */
  acquire: (id: string) => HTMLElement;
  /** Records `node` as the node of `id` (again, after a StrictMode re-run). */
  adopt: (id: string, node: HTMLElement) => void;
  /** Forgets `node` when it is still the node of `id`. */
  release: (id: string, node: HTMLElement) => void;
};

export function createWidgetNodes(): WidgetNodes {
  const nodes = new Map<string, HTMLElement>();

  return {
    acquire(id) {
      let node = nodes.get(id);

      if (!node) {
        node = document.createElement('div');
        node.dataset.widgetNode = id;
        nodes.set(id, node);
      }

      return node;
    },
    adopt(id, node) {
      nodes.set(id, node);
    },
    release(id, node) {
      if (nodes.get(id) === node) {
        nodes.delete(id);
      }
    },
  };
}
