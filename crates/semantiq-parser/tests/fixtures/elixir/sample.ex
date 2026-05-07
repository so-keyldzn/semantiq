defmodule MyApp.User do
  def hello(name) do
    "hi #{name}"
  end

  defp internal(x) do
    x * 2
  end

  defmacro guarded(x), do: x
end

defmodule MyApp.Outer do
  defmodule Inner do
    def deep, do: :ok
  end
end
