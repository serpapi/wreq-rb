# frozen_string_literal: true

require_relative "test_helper"

class RequestTest < Minitest::Test
  def test_simple_get
    resp = Wreq.get("https://httpbin.org/get")
    assert_equal 200, resp.status
    assert resp.success?
    refute resp.text.empty?
  end

  def test_get_with_headers
    resp = Wreq.get("https://httpbin.org/headers",
      headers: { "X-Test-Header" => "hello" })
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "hello", body["headers"]["X-Test-Header"]
  end

  def test_post_json
    resp = Wreq.post("https://httpbin.org/post",
      json: { "name" => "wreq", "version" => 1 })
    assert_equal 200, resp.status
    body = resp.json
    data = body["json"] || JSON.parse(body["data"])
    assert_equal "wreq", data["name"]
  end

  def test_post_form
    resp = Wreq.post("https://httpbin.org/post",
      form: { "key" => "value" })
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "value", body["form"]["key"]
  end

  def test_query_params
    resp = Wreq.get("https://httpbin.org/get",
      query: { "foo" => "bar", "baz" => "qux" })
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "bar", body["args"]["foo"]
    assert_equal "qux", body["args"]["baz"]
  end

  def test_head_request
    resp = Wreq.head("https://httpbin.org/get")
    assert_equal 200, resp.status
  end

  def test_put_request
    resp = Wreq.put("https://httpbin.org/put",
      body: "test body")
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "test body", body["data"]
  end

  def test_delete_request
    resp = Wreq.delete("https://httpbin.org/delete")
    assert_equal 200, resp.status
  end

  def test_patch_request
    resp = Wreq.patch("https://httpbin.org/patch",
      json: { "patched" => true })
    assert_equal 200, resp.status
  end

  def test_bearer_auth
    resp = Wreq.get("https://httpbin.org/bearer",
      bearer: "test-token-123")
    assert_equal 200, resp.status
  end

  def test_basic_auth
    resp = Wreq.get("https://httpbin.org/basic-auth/user/pass",
      basic: ["user", "pass"])
    assert_equal 200, resp.status
  end

  def test_headers_with_mixed_types
    resp = Wreq.get("https://httpbin.org/headers",
      headers: { :"X-Symbol" => 99, "X-Nil" => nil, "X-Str" => "ok" })
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "99", body["headers"]["X-Symbol"]
    assert_nil body["headers"]["X-Nil"]
    assert_equal "ok", body["headers"]["X-Str"]
  end

  def test_headers_override_emulation_defaults
    custom_ua = "MyCustomAgent/1.0"
    resp = Wreq.get("https://httpbin.org/headers",
      emulation: "chrome_145",
      headers: { "User-Agent" => custom_ua })
    assert_equal 200, resp.status
    body = resp.json
    assert_equal custom_ua, body["headers"]["User-Agent"]
  end
end
