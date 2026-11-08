// The mock stack shows a mock-delivery notice in #mock-banner. It is not part of the product tour.
// The admin CSP blocks <style> elements, so this sets the element style through the CSSOM.
export default async function (ctx) {
  await ctx.evaluate("admin", () => {
    document.querySelector("#mock-banner")?.style.setProperty("display", "none", "important");
  });
}
