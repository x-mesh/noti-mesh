// The incident detail is a modal <dialog> in the top layer, which covers the reel-mesh caption.
// Showing the caption overlay as a popover puts it in the top layer after the dialog.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    const overlay = document.querySelector(":root > [data-reel-overlay]");
    if (!overlay) return;
    if (!overlay.popover) overlay.popover = "manual";
    if (overlay.matches(":popover-open")) overlay.hidePopover();
    overlay.showPopover();
  });
}
