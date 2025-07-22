import { DOMAttributes, MutableRefObject } from "react";

type CustomElement<T> = Partial<
  T & DOMAttributes<T> & { children: any } & MutableRefObject
>;
