// reel-mesh has no select action, so the owner is chosen here; the assign button is clicked in the scenario.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    const select = document.querySelector("#assign-user");
    const option = [...select.options].find((o) => o.value === "dev-operator") ?? [...select.options].find((o) => o.value);
    select.value = option.value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
