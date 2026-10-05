export {};

declare global {
  interface Window {
    appReady: boolean;
    flags: Flags;
  }
  var __BUILD__: string;
  export interface Flags {
    beta: boolean;
  }
  namespace NodeJS {
    interface ProcessEnv {
      API_URL: string;
    }
  }
}
