# frozen_string_literal: true

require_relative "test_helper"

class BatchTest < Minitest::Test
  def test_returns_responses_in_input_order
    client = Wreq::Client.new
    urls = (1..6).map { |i| "https://httpbun.com/status/20#{i % 5}" }
    responses = client.request_batch(urls, concurrency: 6)

    assert_equal urls.size, responses.size
    responses.each { |r| assert_kind_of Wreq::Response, r }
    assert_equal [201, 202, 203, 204, 200, 201], responses.map(&:status)
  end

  def test_empty_array
    client = Wreq::Client.new
    assert_equal [], client.request_batch([])
  end

  def test_defaults_to_low_concurrency
    client = Wreq::Client.new
    responses = client.request_batch(Array.new(3, "https://httpbun.com/get"))
    assert_equal 3, responses.size
    responses.each { |r| assert_equal 200, r.status }
  end

  def test_collects_location_headers_without_following_redirects
    client = Wreq::Client.new(redirect: false)
    urls = %w[
      https://httpbun.com/redirect-to?url=https%3A%2F%2Fexample.com%2Fa
      https://httpbun.com/redirect-to?url=https%3A%2F%2Fexample.com%2Fb
    ]
    responses = client.request_batch(urls, concurrency: 2)

    assert_equal [302, 302], responses.map(&:status)
    assert_equal ["https://example.com/a", "https://example.com/b"],
      responses.map { |r| r.headers["location"].first }
  end

  def test_per_item_errors_do_not_fail_the_batch
    client = Wreq::Client.new
    responses = client.request_batch(
      [
        "https://httpbun.com/get",
        "https://this-host-does-not-exist.invalid/",
        "https://httpbun.com/get"
      ],
      concurrency: 3
    )

    assert_equal 3, responses.size
    assert_kind_of Wreq::Response, responses[0]
    assert_kind_of Wreq::Error, responses[1]
    assert_kind_of Wreq::Response, responses[2]
    refute_empty responses[1].message
  end

  def test_applies_shared_request_options
    client = Wreq::Client.new
    responses = client.request_batch(
      ["https://httpbun.com/headers"],
      concurrency: 1,
      headers: { "X-Batch" => "shared" }
    )
    assert_equal "shared", responses[0].json["headers"]["X-Batch"]
  end

  def test_mixed_specs
    client = Wreq::Client.new
    specs = [
      "https://httpbun.com/get",
      { method: :post, url: "https://httpbun.com/post", json: { a: 1 } },
      { method: :put, url: "https://httpbun.com/put", body: "hello" }
    ]
    responses = client.request_batch(specs, concurrency: 3)

    assert_equal [200, 200, 200], responses.map(&:status)
    assert_equal 1, responses[1].json["json"]["a"]
    assert_equal "hello", responses[2].json["data"]
  end

  def test_per_item_timeout_only_fails_that_item
    client = Wreq::Client.new
    responses = client.request_batch(
      [
        "https://httpbun.com/get",
        { url: "https://httpbun.com/delay/5", timeout: 1 },
        "https://httpbun.com/get"
      ],
      concurrency: 3
    )

    assert_kind_of Wreq::Response, responses[0]
    assert_kind_of Wreq::Error, responses[1]
    assert_kind_of Wreq::Response, responses[2]
    assert_match(/time/i, responses[1].message)
  end

  def test_shared_timeout_applies_to_every_request
    client = Wreq::Client.new
    responses = client.request_batch(
      Array.new(2, "https://httpbun.com/delay/5"),
      concurrency: 2,
      timeout: 1
    )

    assert_equal 2, responses.size
    responses.each { |r| assert_kind_of Wreq::Error, r }
  end

  def test_per_item_timeout_overrides_the_shared_one
    client = Wreq::Client.new
    responses = client.request_batch(
      [
        "https://httpbun.com/delay/3",
        { url: "https://httpbun.com/delay/3", timeout: 10 }
      ],
      concurrency: 2,
      timeout: 1
    )

    assert_kind_of Wreq::Error, responses[0]
    assert_kind_of Wreq::Response, responses[1]
  end

  # The timeout clock must start when a request is dispatched, not when it is
  # enqueued, or anything queued behind the semaphore would fail spuriously.
  def test_timeout_clock_starts_at_dispatch_not_enqueue
    client = Wreq::Client.new
    responses = client.request_batch(
      Array.new(4, "https://httpbun.com/delay/1"),
      concurrency: 1,
      timeout: 3
    )

    assert_equal [200, 200, 200, 200], responses.map(&:status)
  end

  def test_rejects_zero_concurrency
    client = Wreq::Client.new
    err = assert_raises(Wreq::Error) do
      client.request_batch(["https://httpbun.com/get"], concurrency: 0)
    end
    assert_match(/concurrency/, err.message)
  end

  def test_rejects_hash_without_url
    client = Wreq::Client.new
    assert_raises(Wreq::Error) do
      client.request_batch([{ method: "GET" }])
    end
  end

  def test_batch_is_cancellable
    client = Wreq::Client.new
    t = Thread.new do
      client.request_batch(Array.new(4, "https://httpbun.com/delay/10"), concurrency: 4)
    end
    sleep 1
    client.cancel

    err = assert_raises(Wreq::Error) { t.value }
    assert_match(/interrupted/, err.message)
  end

  def test_batch_releases_the_gvl
    client = Wreq::Client.new
    ticks = 0
    ticker = Thread.new do
      loop do
        ticks += 1
        sleep 0.05
      end
    end

    client.request_batch(Array.new(4, "https://httpbun.com/delay/1"), concurrency: 4)
    ticker.kill

    assert_operator ticks, :>, 5, "GVL appears to have been held for the whole batch"
  end
end
