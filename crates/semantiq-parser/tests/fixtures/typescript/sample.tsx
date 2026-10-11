import { useState } from "react";
import { Button } from "./Button";
import * as Ui from "./ui";
export { Footer } from "./Footer";
export * from "./hooks";

interface Props {
    user: string;
}

function format(user: string): string {
    return user.toUpperCase();
}

function save(user: string): void {
    console.log(user);
}

export function App({ user }: Props) {
    const [count, setCount] = useState(0);
    const handle = () => save(user);
    return (
        <div className="app">
            <Button onClick={handle} label="Go" />
            <Ui.Card title={format(user)}>
                <p>{count}</p>
            </Ui.Card>
            <button onClick={() => setCount(count + 1)}>+</button>
        </div>
    );
}

export default function Panel() {
    return <App user="x" />;
}

export const Unused = () => null;
