defmodule Ockam.Identity.SecureChannel.Tests do
  use ExUnit.Case

  alias Ockam.Identity
  alias Ockam.Identity.SecureChannel

  alias Ockam.Vault.Software, as: SoftwareVault

  require Logger

  @identity_impl Ockam.Identity.Stub

  test "local secure channel" do
    {:ok, vault} = SoftwareVault.init()
    {:ok, alice, alice_id} = Identity.create(@identity_impl)

    {:ok, listener} =
      SecureChannel.create_listener(
        identity: alice,
        encryption_options: [vault: vault]
      )

    {:ok, bob, bob_id} = Identity.create(@identity_impl)

    {:ok, channel} =
      SecureChannel.create_channel(
        identity: bob,
        encryption_options: [vault: vault],
        route: [listener]
      )

    {:ok, me} = Ockam.Node.register_random_address()
    Logger.info("Channel: #{inspect(channel)} me: #{inspect(me)}")
    Ockam.Router.route("PING!", [channel, me], [me])

    assert_receive %Ockam.Message{
      onward_route: [^me],
      payload: "PING!",
      return_route: return_route,
      local_metadata: %{identity: id, channel: :identity_secure_channel}
    }

    assert id == bob_id

    Ockam.Router.route("PONG!", return_route, [me])

    assert_receive %Ockam.Message{
      onward_route: [^me],
      payload: "PONG!",
      return_route: [^channel | _],
      local_metadata: %{identity: id, channel: :identity_secure_channel}
    }

    assert id == alice_id
  end

  test "identity channel inner address is protected" do
    {:ok, vault} = SoftwareVault.init()
    {:ok, alice, _alice_id} = Identity.create(@identity_impl)

    {:ok, listener} =
      SecureChannel.create_listener(
        identity: alice,
        encryption_options: [vault: vault]
      )

    {:ok, bob, _bob_id} = Identity.create(@identity_impl)

    {:ok, channel} =
      SecureChannel.create_channel(
        identity: bob,
        encryption_options: [vault: vault],
        route: [listener]
      )

    {:ok, bob_inner_address} = Ockam.AsymmetricWorker.get_inner_address(channel)

    {:ok, me} = Ockam.Node.register_random_address()

    Ockam.Router.route("PING!", [bob_inner_address, me], [me])

    refute_receive %Ockam.Message{
      onward_route: [^me],
      payload: "PING!"
    }
  end
end