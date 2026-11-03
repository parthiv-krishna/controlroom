// The one pi-tui export the renderers use; pi provides the module at runtime.
declare module '@earendil-works/pi-tui' {
  export class Text {
    constructor(text?: string, paddingX?: number, paddingY?: number);
  }
}
