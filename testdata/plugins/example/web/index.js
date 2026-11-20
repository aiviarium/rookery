export default {
  views: {
    notes(context, element) {
      const status = document.createElement("p");
      status.textContent = "Loading notes…";
      element.replaceChildren(status);
      const controller = new AbortController();
      context.rookery.data.find(
        { collection: "example/notes" },
        { signal: controller.signal },
      ).then(({ recordsCbor }) => {
        status.textContent = `${recordsCbor.length} notes`;
      }).catch(() => {
        if (!controller.signal.aborted) status.textContent = "Notes could not be loaded.";
      });
      return () => controller.abort();
    },
  },
  hostTabs: {
    hello(_context, element, host) {
      element.textContent = `Hello, ${host}`;
    },
  },
};
