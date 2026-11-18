# frozen_string_literal: true

require_relative "test_helper"

class EmulationTest < Minitest::Test
  def test_default_emulation_uses_chrome_ua
    resp = Wreq.get("https://httpbun.com/headers")
    body = resp.json
    assert_match(/Chrome/, body["headers"]["User-Agent"])
  end

  def test_explicit_emulation_firefox
    client = Wreq::Client.new(emulation: "firefox_146")
    resp = client.get("https://httpbun.com/headers")
    body = resp.json
    assert_match(/Firefox/, body["headers"]["User-Agent"])
  end

  def test_emulation_firefox_releases_newer_than_wreq_util
    (152..158).each do |version|
      client = Wreq::Client.new(emulation: "firefox_#{version}")
      headers = client.get("https://httpbun.com/headers").json["headers"]
      assert_match(%r{rv:#{version}\.0\) Gecko/20100101 Firefox/#{version}\.0\z}, headers["User-Agent"])
      assert_equal "en-US,en;q=0.9", headers["Accept-Language"]
    end
  end

  def test_emulation_firefox_158_per_request_and_os
    client = Wreq::Client.new(emulation: false)
    resp = client.get("https://httpbun.com/headers", emulation: "firefox_158", emulation_os: "windows")
    assert_equal "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:158.0) Gecko/20100101 Firefox/158.0",
                 resp.json["headers"]["User-Agent"]
  end

  def test_emulation_unknown_firefox_release
    error = assert_raises(Wreq::Error) { Wreq::Client.new(emulation: "firefox_999") }
    assert_match(/unknown emulation: 'firefox_999'/, error.message)
  end

  def test_emulation_disabled
    client = Wreq::Client.new(emulation: false, user_agent: "custom-agent/1.0")
    resp = client.get("https://httpbun.com/headers")
    body = resp.json
    assert_equal "custom-agent/1.0", body["headers"]["User-Agent"]
  end

  def test_emulation_with_user_agent_override
    client = Wreq::Client.new(emulation: "chrome_143", user_agent: "my-bot/2.0")
    resp = client.get("https://httpbun.com/headers")
    body = resp.json
    assert_equal "my-bot/2.0", body["headers"]["User-Agent"]
  end

  def test_emulation_os_windows
    client = Wreq::Client.new(emulation: "chrome_145", emulation_os: "windows")
    resp = client.get("https://httpbun.com/headers")
    body = resp.json
    headers = body["headers"]
    assert_match(/Windows/, headers["Sec-Ch-Ua-Platform"])
  end

  def test_emulation_os_macos_default
    client = Wreq::Client.new(emulation: "chrome_145")
    resp = client.get("https://httpbun.com/headers")
    body = resp.json
    headers = body["headers"]
    assert_match(/macOS/, headers["Sec-Ch-Ua-Platform"])
  end

  def test_emulation_os_invalid
    assert_raises(Wreq::Error) do
      Wreq::Client.new(emulation: "chrome_145", emulation_os: "beos")
    end
  end
end
