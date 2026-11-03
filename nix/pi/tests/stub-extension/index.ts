// Test stand-in for @controlroom/pi: proves the bundle's extension loads.
export default function (pi: { registerCommand(name: string, options: object): void }) {
  pi.registerCommand("controlroom", {
    description: "controlroom extension (test stub)",
    handler: async () => {},
  });
}
