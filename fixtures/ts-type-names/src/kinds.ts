export interface Fee {
  cents: number;
}

export namespace Fee {
  export type Code = string;
}

export const enum Flag {
  On,
  Off,
}
