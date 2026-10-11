defmodule MyApp.User do
  def hello(name) do
    "hi #{name}"
  end

  defp internal(x) do
    x * 2
  end

  defmacro guarded(x), do: x

  def positive?(x) when is_integer(x), do: x > 0

  def greet(name) do
    hello(name)
  end
end

defmodule MyApp.Outer do
  defmodule Inner do
    def deep, do: :ok
  end
end
