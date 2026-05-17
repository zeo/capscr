import { JSX } from "solid-js";

interface Props {
  num: string; // "01"
  title: string;
  desc?: string;
  children: JSX.Element;
}

export function Section(props: Props) {
  return (
    <section class="section">
      <div class="section-head">
        <span class="num">{props.num}</span>
        <span class="title">{props.title}</span>
        {props.desc && <span class="desc">{props.desc}</span>}
      </div>
      {props.children}
    </section>
  );
}
